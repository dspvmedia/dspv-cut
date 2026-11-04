//! ISO BMFF (MP4) and QuickTime (MOV) demuxer and muxer: a re-export of
//! [`craft_media::isobmff`] from craft-libs. Supported boxes and limitations are in the
//! craft-media README.
//!
//! DSPV fork: [`open`] is our own thin layer over craft-media's (see its documentation): it reads QuickTime sound
//! descriptions version 2 in files that are not branded `qt  ` (Nikon N-RAW), and refuses unusable version 2 values.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub use craft_media::isobmff::*;

/// Open an MP4 / MOV file: craft-media's [`craft_media::isobmff::open`], plus two DSPV rules.
///
/// * **QuickTime sound description version 2 in any brand.** craft-media reads the version 2 fields only when the major brand
///   is `qt  `. ISO base media defines no version 2, so the version alone identifies this layout; Nikon N-RAW (`.NEV`) files are
///   branded `niko`, and without this their 48 kHz stereo 24-bit audio read as 1 Hz, 3 channels. When a file that is not
///   QuickTime has a version 2 sound entry, it is read again with its `ftyp` major brand shown as `qt  ` (craft-media uses the
///   brand for nothing else than the version 1 / 2 sound fields), and the real brand is put back on the result.
/// * **Unusable version 2 values are an error.** The sample rate is a raw f64 from the file: NaN, infinite, zero, negative or
///   beyond `u32` would reach `as u32` casts and rate-ratio divisions in the audio path. Bits per channel are held to the
///   16-bit ceiling of the version 0 layout (craft-media already holds the channel count to it).
pub fn open<S: ByteSource>(src: S) -> Result<Mp4File> {
    let mut file = craft_media::isobmff::open(&src)?;
    if !file.is_quicktime && has_v2_sound(&file) && starts_with_ftyp(&src) {
        let (brand, quicktime) = (file.major_brand, file.is_quicktime);
        file = craft_media::isobmff::open(AsQuickTime(&src))?;
        file.major_brand = brand;
        file.is_quicktime = quicktime;
    }
    check_v2_sound(&file)?;
    Ok(file)
}

fn v2_sound(file: &Mp4File) -> impl Iterator<Item = &AudioParams> {
    file.tracks.iter().flat_map(|t| t.entries.iter()).filter_map(|e| e.audio.as_ref()).filter(|a| a.qt_version == 2)
}

fn has_v2_sound(file: &Mp4File) -> bool {
    v2_sound(file).next().is_some()
}

fn check_v2_sound(file: &Mp4File) -> Result<()> {
    for a in v2_sound(file) {
        if !(a.sample_rate > 0.0 && a.sample_rate <= f64::from(u32::MAX)) {
            return Err(Error::Invalid("sound description version 2 sample rate".into()));
        }
        if a.const_bits_per_channel > u32::from(u16::MAX) {
            return Err(Error::TooLarge("sound description bits per channel"));
        }
        if a.channels > u32::from(u16::MAX) {
            return Err(Error::TooLarge("sound description channel count"));
        }
    }
    Ok(())
}

/// The file opens with an `ftyp` box (size, then the type at bytes 4..8): only then are bytes 8..12 its major brand.
fn starts_with_ftyp<S: ByteSource + ?Sized>(src: &S) -> bool {
    let mut head = [0u8; 8];
    src.len() >= 12 && src.read_at(0, &mut head).is_ok() && &head[4..8] == b"ftyp"
}

/// A byte source whose `ftyp` major brand (bytes 8..12) reads as `qt  `; every other byte is the file's own.
struct AsQuickTime<S>(S);

impl<S: ByteSource> ByteSource for AsQuickTime<S> {
    fn len(&self) -> u64 {
        self.0.len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_at(offset, buf)?;
        for (i, b) in b"qt  ".iter().enumerate() {
            let at = 8 + i as u64;
            if let Some(slot) = at.checked_sub(offset).and_then(|k| usize::try_from(k).ok()).and_then(|k| buf.get_mut(k)) {
                *slot = *b;
            }
        }
        Ok(())
    }
}
