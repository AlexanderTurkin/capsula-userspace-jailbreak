//! Wire-format tests of the Amlogic update protocol on MockTransport:
//! every step is checked byte-for-byte against the device side
//! (u-boot v2_burning usb_pcd.c) and pyamlboot.

use amlogic::req;
use amlogic::session::{BULK_REPLY_LEN, MEDIA_ACK_LEN, MEDIA_CHUNK};
use amlogic::{soc::Stage, AmlogicSession, SocId};
use usbcore::mock::{MockTransport, Step};

fn session(steps: Vec<Step>) -> AmlogicSession<MockTransport> {
    AmlogicSession::new(MockTransport::new(steps), 0x81, 0x02)
}

/// identify: control IN 0x20, 8 bytes requested, reply parsed to SocId.
#[test]
fn identify_wire_format() {
    let mut s = session(vec![Step::ControlIn(
        req::IDENTIFY_HOST,
        0,
        0,
        vec![0x00, 0x20, 0x01, 0x10, 0, 0, 0, 0],
    )]);
    let id = s.identify().unwrap();
    assert_eq!(
        id,
        SocId::parse(&[0x00, 0x20, 0x01, 0x10, 0, 0, 0, 0]).unwrap()
    );
    assert_eq!(id.stage(), Stage::Tpl);
    assert_eq!(id.version(), "0-32-1-16");
    assert!(s.transport().log[0].contains("ctrl-in 0x20 8b"));
}

/// BootROM identify: a short 4-byte reply is valid.
#[test]
fn identify_short_rom_reply() {
    let mut s = session(vec![Step::ControlIn(
        req::IDENTIFY_HOST,
        0,
        0,
        vec![0x00, 0x09, 0x00, 0x00],
    )]);
    let id = s.identify().unwrap();
    assert_eq!(id.stage(), Stage::Ipl);
}

/// Simple memory write: wValue/wIndex carry the address, data rides the
/// EP0 stage; runs are chunked by 64.
#[test]
fn write_memory_chunks_wire_format() {
    let data: Vec<u8> = (0..100u8).collect();
    let mut steps = vec![];
    for (i, chunk) in data.chunks(64).enumerate() {
        let addr = 0xd9000010u32 + (i * 64) as u32;
        steps.push(Step::ControlOut(
            req::WRITE_MEM,
            (addr >> 16) as u16,
            addr as u16,
            chunk.to_vec(),
        ));
    }
    let mut s = session(steps);
    s.write_memory_chunks(0xd9000010, &data).unwrap();
    assert_eq!(s.transport().pos, 2); // both chunks consumed
}

/// Simple memory read: control IN 0x02 at the address.
#[test]
fn read_memory_wire_format() {
    let mut s = session(vec![Step::ControlIn(
        req::READ_MEM,
        0xc801,
        0x3c24,
        vec![0x11, 0x22, 0x33, 0x44],
    )]);
    let mut buf = [0u8; 4];
    let n = s.read_memory(0xc8013c24, &mut buf).unwrap();
    assert_eq!(n, 4);
    assert_eq!(buf, [0x11, 0x22, 0x33, 0x44]);
}

/// Run: 4-byte LE address in the EP0 data, keep-power flag ORed in.
#[test]
fn run_wire_format() {
    let mut s = session(vec![
        Step::ControlOut(
            req::RUN_IN_ADDR,
            0x0100,
            0x8000,
            0x01008010u32.to_le_bytes().to_vec(),
        ),
        Step::ControlOut(
            req::RUN_IN_ADDR,
            0x0100,
            0x8000,
            0x01008000u32.to_le_bytes().to_vec(),
        ),
    ]);
    s.run(0x01008000, true).unwrap();
    s.run(0x01008000, false).unwrap();
}

/// Large write: control 0x11 with (block_len, block_count) and the
/// 16-byte head [addr, len], then the payload on bulk OUT.
#[test]
fn write_large_memory_wire_format() {
    let mut s = session(vec![
        Step::ControlOut(req::WR_LARGE_MEM, 0x0200, 3, {
            let mut h = [0u8; 16];
            h[0..4].copy_from_slice(&0x01000000u32.to_le_bytes());
            h[4..8].copy_from_slice(&0x0600u32.to_le_bytes());
            h.to_vec()
        }),
        Step::BulkOut((0u32..0x200).map(|b| b as u8).collect()),
        Step::BulkOut((0x200u32..0x400).map(|b| b as u8).collect()),
        Step::BulkOut({
            // the tail chunk is zero-padded to a whole block
            let mut c = vec![0u8; 0x200];
            c[0] = 0x55;
            c
        }),
    ]);
    let data: Vec<u8> = (0u32..0x200)
        .chain(0x200u32..0x400)
        .map(|b| b as u8)
        .chain([0x55])
        .collect();
    s.write_large_memory(0x01000000, &data, 0x200).unwrap();
    assert_eq!(s.transport().pos, 4);
}

/// Large read: control 0x12 announces, payload arrives on bulk IN.
#[test]
fn read_large_memory_wire_format() {
    let mut s = session(vec![
        Step::ControlOut(req::RD_LARGE_MEM, 0x0200, 2, {
            let mut h = [0u8; 16];
            h[0..4].copy_from_slice(&0xd9000000u32.to_le_bytes());
            h[4..8].copy_from_slice(&0x0400u32.to_le_bytes());
            h.to_vec()
        }),
        Step::BulkIn(vec![0x11; 0x200]),
        Step::BulkIn(vec![0x22; 0x200]),
    ]);
    let mut buf = vec![0u8; 0x400];
    s.read_large_memory(0xd9000000, &mut buf, 0x200).unwrap();
    assert_eq!(&buf[..0x200], &vec![0x11; 0x200][..]);
    assert_eq!(&buf[0x200..], &vec![0x22; 0x200][..]);
}

/// bulkcmd: NUL-terminated string in the EP0 stage (wValue=0, wIndex=2),
/// 512-byte status on bulk IN; `success` is accepted, other text is an
/// error.
#[test]
fn bulk_cmd_wire_format() {
    let mut s = session(vec![
        Step::ControlOut(req::BULKCMD, 0, 2, b"low_power\0".to_vec()),
        Step::BulkIn({
            let mut r = vec![0u8; BULK_REPLY_LEN];
            r[..7].copy_from_slice(b"success");
            r
        }),
    ]);
    let status = s.bulk_cmd("low_power").unwrap();
    assert_eq!(status, "success");
}

#[test]
fn bulk_cmd_error_text_surfaces() {
    let mut s = session(vec![
        Step::ControlOut(req::BULKCMD, 0, 2, b"store rom_write x\0".to_vec()),
        Step::BulkIn({
            let mut r = vec![0u8; BULK_REPLY_LEN];
            r[..9].copy_from_slice(b"FAILED: 1");
            r
        }),
    ]);
    let err = s.bulk_cmd("store rom_write x").unwrap_err();
    assert!(err.to_string().contains("FAILED: 1"));
}

#[test]
fn bulk_cmd_too_long_rejected_before_the_bus() {
    let long = "x".repeat(200);
    let mut s = session(vec![]);
    let err = s.bulk_cmd(&long).unwrap_err();
    assert!(err.to_string().contains("command too long"));
    assert_eq!(s.transport().pos, 0); // nothing was sent
}

/// Media write (download packet): 32-byte head (retry, len, seq, add_sum,
/// alg 0x00ef, ack len 0x200), payload on bulk OUT, `OK!!` ack on bulk IN.
#[test]
fn write_media_wire_format() {
    let data = vec![0x42u8; 512];
    let sum = amlogic::add_sum(&data);
    let mut head = [0u8; 0x20];
    head[4..8].copy_from_slice(&512u32.to_le_bytes());
    head[8..12].copy_from_slice(&7u32.to_le_bytes());
    head[12..16].copy_from_slice(&sum.to_le_bytes());
    head[16..18].copy_from_slice(&0x00efu16.to_le_bytes());
    head[18..20].copy_from_slice(&(MEDIA_ACK_LEN as u16).to_le_bytes());

    let mut s = session(vec![
        Step::ControlOut(req::WRITE_MEDIA, 1, 0xffff, head.to_vec()),
        Step::BulkOut(data.clone()),
        Step::BulkIn({
            let mut r = vec![0u8; MEDIA_ACK_LEN];
            r[..4].copy_from_slice(b"OK!!");
            r
        }),
    ]);
    s.write_media(&data, 7).unwrap();
    assert_eq!(s.transport().pos, 3);
}

#[test]
fn write_media_rejects_error_ack() {
    let data = vec![1u8; 16];
    let sum = amlogic::add_sum(&data);
    let mut head = [0u8; 0x20];
    head[4..8].copy_from_slice(&16u32.to_le_bytes());
    head[12..16].copy_from_slice(&sum.to_le_bytes());
    head[16..18].copy_from_slice(&0x00efu16.to_le_bytes());
    head[18..20].copy_from_slice(&(MEDIA_ACK_LEN as u16).to_le_bytes());

    let mut s = session(vec![
        Step::ControlOut(req::WRITE_MEDIA, 1, 0xffff, head.to_vec()),
        Step::BulkOut(data),
        Step::BulkIn({
            let mut r = vec![0u8; MEDIA_ACK_LEN];
            r[..15].copy_from_slice(b"0x: genSum 5 !=");
            r
        }),
    ]);
    let err = s.write_media(&[1u8; 16], 0).unwrap_err();
    assert!(err.to_string().contains("genSum"));
}

/// Media streaming chunks a large buffer into MEDIA_CHUNK packets.
#[test]
fn write_media_all_chunks() {
    let data = vec![0x77u8; MEDIA_CHUNK + 128];
    let mut steps = vec![];
    for (seq, part) in [(0u32, &data[..MEDIA_CHUNK]), (1u32, &data[MEDIA_CHUNK..])] {
        let sum = amlogic::add_sum(part);
        let mut head = [0u8; 0x20];
        head[4..8].copy_from_slice(&(part.len() as u32).to_le_bytes());
        head[8..12].copy_from_slice(&seq.to_le_bytes());
        head[12..16].copy_from_slice(&sum.to_le_bytes());
        head[16..18].copy_from_slice(&0x00efu16.to_le_bytes());
        head[18..20].copy_from_slice(&(MEDIA_ACK_LEN as u16).to_le_bytes());
        steps.push(Step::ControlOut(req::WRITE_MEDIA, 1, 0xffff, head.to_vec()));
        steps.push(Step::BulkOut(part.to_vec()));
        steps.push(Step::BulkIn({
            let mut r = vec![0u8; MEDIA_ACK_LEN];
            r[..4].copy_from_slice(b"OK!!");
            r
        }));
    }
    let mut s = session(steps);
    s.write_media_all(&data).unwrap();
}

/// Upload: 16-byte EP0 header reply, then the payload on bulk IN.
#[test]
fn read_media_wire_format() {
    let mut s = session(vec![
        Step::ControlIn(req::READ_MEDIA, 4, 1, vec![0u8; 16]),
        Step::BulkIn(0x12345678u32.to_le_bytes().to_vec()),
    ]);
    let data = s.read_media(4).unwrap();
    assert_eq!(data, 0x12345678u32.to_le_bytes());
}

/// The full upload_read flow: bulkcmd `upload mem ...` + status + media.
#[test]
fn upload_read_flow() {
    let payload = vec![0x5au8; 0x10];
    let mut s = session(vec![
        Step::ControlOut(
            req::BULKCMD,
            0,
            2,
            b"upload mem 0x1000000 normal 0x10\0".to_vec(),
        ),
        Step::BulkIn({
            let mut r = vec![0u8; BULK_REPLY_LEN];
            r[..7].copy_from_slice(b"success");
            r
        }),
        Step::ControlIn(req::READ_MEDIA, 0x10, 1, vec![0u8; 16]),
        Step::BulkIn(payload.clone()),
    ]);
    let got = s.upload_read(0x1000000, 0x10).unwrap();
    assert_eq!(got, payload);
}

/// fill memory: pairs of (addr, value) LE words in the EP0 stage.
#[test]
fn fill_memory_wire_format() {
    let mut s = session(vec![Step::ControlOut(req::FILL_MEM, 0, 0, {
        let mut d = Vec::new();
        d.extend_from_slice(&0xffd00000u32.to_le_bytes());
        d.extend_from_slice(&0x1234u32.to_le_bytes());
        d
    })]);
    s.fill_memory(&[(0xffd00000, 0x1234)]).unwrap();
}

/// The bulkcmd payload is capped at 63 chars + NUL: the device truncates
/// at 64 silently (flasher.sh BULKCMD_MAX).
#[test]
fn bulk_cmd_payload_cap() {
    let mut s = session(vec![Step::ControlOut(req::BULKCMD, 0, 2, {
        let mut v = vec![b'x'; 63];
        v.push(0);
        v
    })]);
    // 63 chars + NUL = exactly 64 bytes: fits, and reaches the bus
    assert!(s.send_bulk_cmd(&"x".repeat(63)).is_ok());
    assert_eq!(s.transport().pos, 1);
    // 64 chars would be truncated by the device: refused before the bus
    let err = s.send_bulk_cmd(&"x".repeat(64)).unwrap_err();
    assert!(err.to_string().contains("command too long"));
    assert_eq!(s.transport().pos, 1);
}

/// upload_store: one arm, then sequential AM_REQ_UPLOAD fetches until the
/// head reports no data left; a short stream (bad blocks) is kept.
#[test]
fn upload_store_stream() {
    fn head(left: u32, seq: u32) -> Vec<u8> {
        let mut h = vec![0u8; 16];
        h[4..8].copy_from_slice(&left.to_le_bytes());
        h[8..12].copy_from_slice(&seq.to_le_bytes());
        h
    }
    fn ok() -> Vec<u8> {
        let mut r = vec![0u8; BULK_REPLY_LEN];
        r[..7].copy_from_slice(b"success");
        r
    }
    // total 0x15000: fetch 0xffff then 0x5001
    let mut steps = vec![
        Step::ControlOut(
            req::BULKCMD,
            0,
            2,
            b"upload store data normal 0x15000\0".to_vec(),
        ),
        Step::BulkIn(ok()),
        Step::ControlIn(req::READ_MEDIA, 0xffff, 16, head(0x5001, 0)),
        Step::BulkIn(vec![0xa5; 0xffff]),
        Step::ControlIn(req::READ_MEDIA, 0x5001, 6, head(0, 1)),
        Step::BulkIn(vec![0x5a; 0x5001]),
    ];
    let _ = &mut steps;
    let mut s = session(steps);
    let data = s.upload_store("data", 0x15000, &|_| {}).unwrap();
    assert_eq!(data.len(), 0x15000);
    assert_eq!(&data[..4], &[0xa5; 4][..]);
    assert_eq!(&data[0xffff..0xffff + 4], &[0x5a; 4][..]);
}

/// upload_store keeps a short stream when the device reports no data
/// left early (bad blocks shorten NAND reads).
#[test]
fn upload_store_short_read_kept() {
    let mut h = vec![0u8; 16]; // left = 0
    let mut ok = vec![0u8; BULK_REPLY_LEN];
    ok[..7].copy_from_slice(b"success");
    let mut s = session(vec![
        Step::ControlOut(
            req::BULKCMD,
            0,
            2,
            b"upload store data normal 0x20000\0".to_vec(),
        ),
        Step::BulkIn(ok),
        Step::ControlIn(req::READ_MEDIA, 0xffff, 16, std::mem::take(&mut h)),
        Step::BulkIn(vec![0x33; 0xffff]),
    ]);
    let data = s.upload_store("data", 0x20000, &|_| {}).unwrap();
    assert_eq!(data.len(), 0xffff); // short, but kept
}

/// read_dram chunks the range into independent upload-mem arms.
#[test]
fn read_dram_chunks() {
    let mut ok = vec![0u8; BULK_REPLY_LEN];
    ok[..7].copy_from_slice(b"success");
    let mut steps = vec![];
    let chunks = [
        (amlogic::ENV_ADDR, 0x8000usize, 0x11u8),
        (amlogic::ENV_ADDR + 0x8000, 0x8000, 0x22),
    ];
    for (addr, size, fill) in chunks {
        steps.push(Step::ControlOut(
            req::BULKCMD,
            0,
            2,
            format!("upload mem {addr:#x} normal {size:#x}\0").into_bytes(),
        ));
        steps.push(Step::BulkIn(ok.clone()));
        steps.push(Step::ControlIn(
            req::READ_MEDIA,
            size as u16,
            (size / 0x1000) as u16,
            vec![0u8; 16],
        ));
        steps.push(Step::BulkIn(vec![fill; size]));
    }
    let mut s = session(steps);
    let data = s.read_dram(amlogic::ENV_ADDR, 0x10000, &|_| {}).unwrap();
    assert_eq!(&data[..4], &[0x11; 4][..]);
    assert_eq!(&data[0x8000..0x8004], &[0x22; 4][..]);
}

/// nand_read builds the flasher's raw-offset read command.
#[test]
fn nand_read_command() {
    let mut ok = vec![0u8; BULK_REPLY_LEN];
    ok[..7].copy_from_slice(b"success");
    let mut s = session(vec![
        Step::ControlOut(
            req::BULKCMD,
            0,
            2,
            b"nand read 0x1080000 0xb200000 0x8000\0".to_vec(),
        ),
        Step::BulkIn(ok),
    ]);
    s.nand_read(0x0108_0000, 0x0b20_0000, 0x8000).unwrap();
}
