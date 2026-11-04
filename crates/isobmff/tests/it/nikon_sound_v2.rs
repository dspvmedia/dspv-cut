//! DSPV: QuickTime sound description version 2 in a file that is not branded `qt  ` (Nikon N-RAW `.NEV` files are branded
//! `niko`), and the version 2 values the audio path cannot use. See `filmcraft_isobmff::open`.

use filmcraft_isobmff::*;
use std::io::Cursor;

/// A MOV with one 48 kHz, 2-channel, 24-bit little-endian PCM track (the muxer writes it as an `lpcm` version 2 entry, like a
/// Nikon N-RAW file), with its major brand replaced by `brand`.
fn pcm_v2_file(brand: &[u8; 4]) -> Vec<u8> {
    let pcm = PcmConfig { bits: 24, float: false, big_endian: false, signed: true, channels: 2, sample_rate: 48_000.0 };
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
    let a = w.add_track(TrackConfig::new(SampleEntry::pcm(pcm), 48_000)).unwrap();
    w.write_sample(a, WriteSample { data: &[0u8; 6 * 480], duration: 480, composition_offset: 0, is_sync: true }).unwrap();
    let mut b = w.finish().unwrap().into_inner();
    assert_eq!(&b[4..8], b"ftyp");
    b[8..12].copy_from_slice(brand);
    b
}

/// Where the `lpcm` entry's version 2 sample rate (a big-endian f64) is: 32 bytes into the entry's body.
fn rate_at(b: &[u8]) -> usize {
    b.windows(4).position(|w| w == b"lpcm").expect("an lpcm entry") + 4 + 32
}

fn pcm(f: &Mp4File) -> PcmConfig {
    let t = f.tracks.iter().find(|t| t.kind == TrackKind::Audio).expect("an audio track");
    let e = t.entries.first().expect("a sample entry");
    assert_eq!(e.audio.as_ref().map(|a| a.qt_version), Some(2));
    match &e.codec {
        CodecConfig::Pcm(p) => *p,
        other => panic!("expected PCM, got {other:?}"),
    }
}

#[test]
fn quicktime_sound_description_v2_is_read_in_a_nikon_branded_file() {
    let f = open(pcm_v2_file(b"niko").as_slice()).expect("opens");
    let p = pcm(&f);
    assert_eq!((p.channels, p.sample_rate as u32, p.bits, p.big_endian, p.signed, p.float), (2, 48_000, 24, false, true, false));
    // the file's own brand is reported, not the one used to read it
    assert_eq!((&f.major_brand.0, f.is_quicktime), (b"niko", false));
    // a QuickTime-branded file reads the same
    let q = open(pcm_v2_file(b"qt  ").as_slice()).expect("opens");
    assert_eq!(pcm(&q), p);
    assert!(q.is_quicktime);
}

#[test]
fn sound_description_v2_with_an_unusable_sample_rate_is_rejected() {
    for brand in [b"niko", b"qt  "] {
        for rate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -48_000.0, 1e12] {
            let mut b = pcm_v2_file(brand);
            let at = rate_at(&b);
            b[at..at + 8].copy_from_slice(&rate.to_be_bytes());
            assert!(open(b.as_slice()).is_err(), "{} rate {rate}", String::from_utf8_lossy(brand));
        }
        let mut b = pcm_v2_file(brand);
        let at = rate_at(&b);
        b[at..at + 8].copy_from_slice(&192_000f64.to_be_bytes());
        assert_eq!(pcm(&open(b.as_slice()).expect("opens")).sample_rate as u32, 192_000);
    }
}

#[test]
fn sound_description_v2_with_absurd_channel_or_bit_counts_is_rejected() {
    // channels is the u32 8 bytes after the rate, bits per channel 16 bytes after it
    for (off, v) in [(8usize, u32::MAX), (8, 65_536), (16, u32::MAX), (16, 65_536)] {
        let mut b = pcm_v2_file(b"niko");
        let at = rate_at(&b) + off;
        b[at..at + 4].copy_from_slice(&v.to_be_bytes());
        assert!(open(b.as_slice()).is_err(), "offset {off} value {v}");
    }
}

#[test]
fn version_zero_entries_in_non_quicktime_files_are_unaffected() {
    // 16-bit PCM in MP4: an `ipcm` version 0 entry, read as before
    let pcm16 = PcmConfig { bits: 16, float: false, big_endian: false, signed: true, channels: 2, sample_rate: 48_000.0 };
    let mut w = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mp4)).unwrap();
    let a = w.add_track(TrackConfig::new(SampleEntry::pcm(pcm16), 48_000)).unwrap();
    w.write_sample(a, WriteSample { data: &[0u8; 4 * 480], duration: 480, composition_offset: 0, is_sync: true }).unwrap();
    let f = open(w.finish().unwrap().into_inner().as_slice()).expect("opens");
    let e = &f.tracks[0].entries[0];
    assert_eq!(e.audio.as_ref().map(|a| a.qt_version), Some(0));
    assert!(!f.is_quicktime);
}

#[test]
fn truncated_and_corrupt_nikon_files_never_panic() {
    let b = pcm_v2_file(b"niko");
    for n in 0..=b.len() {
        let _ = open(&b[..n]);
    }
    let entry = b.windows(4).position(|w| w == b"lpcm").expect("lpcm");
    for i in entry.saturating_sub(8)..(entry + 72).min(b.len()) {
        for v in [0u8, 0x7f, 0xff] {
            let mut c = b.clone();
            c[i] = v;
            let _ = open(c.as_slice());
        }
    }
}
