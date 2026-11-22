//! Reading NZB files (plain or gzipped): which articles make up which file.

use std::io::Read;

use anyhow::{Context, Result, bail};

/// One file posted to Usenet.
#[derive(Clone, Debug, PartialEq)]
pub struct NzbFile {
    /// The file name from the post's subject.
    pub name: String,
    /// In order, without duplicates.
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub number: u32,
    /// Encoded size, roughly 2% more than the data.
    pub bytes: u32,
    pub message_id: String,
}

impl NzbFile {
    pub fn encoded_bytes(&self) -> u64 {
        self.segments.iter().map(|s| u64::from(s.bytes)).sum()
    }

    pub fn is_par2(&self) -> bool {
        self.name.to_ascii_lowercase().ends_with(".par2")
    }

    /// The PAR2 set a .par2 file belongs to, by name: "Show - 01.par2" and
    /// "Show - 01.vol03+04.par2" are both set "Show - 01".
    pub fn par2_set(&self) -> Option<&str> {
        let lower = self.name.to_ascii_lowercase();
        let stem_len = lower.strip_suffix(".par2")?.len();
        let stem = &lower[..stem_len];
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        match stem.rfind(".vol") {
            Some(i)
                if stem[i + 4..]
                    .split_once('+')
                    .is_some_and(|(a, b)| digits(a) && digits(b)) =>
            {
                Some(&self.name[..i])
            }
            _ => Some(&self.name[..stem_len]),
        }
    }

    /// For a PAR2 recovery volume (`name.vol03+04.par2`), how many recovery
    /// blocks it holds; `None` for the index file or anything else.
    pub fn recovery_blocks(&self) -> Option<u32> {
        let lower = self.name.to_ascii_lowercase();
        let stem = lower.strip_suffix(".par2")?;
        let vol = &stem[stem.rfind(".vol")? + 4..];
        vol.split_once('+')?.1.parse().ok()
    }
}

/// Parse an NZB, gzipped or not.
pub fn parse(bytes: &[u8]) -> Result<Vec<NzbFile>> {
    let text = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = String::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_string(&mut out)
            .context("could not unpack the gzipped NZB")?;
        out
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    let nzb = nzb_rs::Nzb::parse(&text).map_err(|e| anyhow::anyhow!("invalid NZB: {e}"))?;
    let mut files: Vec<NzbFile> = Vec::new();
    for f in nzb.files {
        let Some(name) = f.name().map(str::to_owned) else {
            continue;
        };
        let mut segments: Vec<Segment> = f
            .segments
            .iter()
            .map(|s| Segment {
                number: s.number,
                bytes: s.size,
                message_id: s.message_id.trim_matches(['<', '>']).to_owned(),
            })
            .collect();
        segments.sort_by_key(|s| s.number);
        segments.dedup_by_key(|s| s.number);
        files.push(NzbFile { name, segments });
    }
    if files.is_empty() {
        bail!("the NZB lists no files");
    }
    Ok(files)
}

/// Write an NZB for `files` (tests, and the fake provider).
#[cfg(test)]
pub fn write(files: &[NzbFile]) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n",
    );
    let n = files.len();
    for (i, f) in files.iter().enumerate() {
        let subject = format!(
            "[{}/{n}] - \"{}\" yEnc (1/{})",
            i + 1,
            f.name,
            f.segments.len()
        );
        out.push_str(&format!(
            "<file poster=\"Fake &lt;fake@example.invalid&gt;\" date=\"1795000000\" subject=\"{}\">\n\
             <groups><group>alt.binaries.test</group></groups>\n<segments>\n",
            esc(&subject)
        ));
        for s in &f.segments {
            out.push_str(&format!(
                "<segment bytes=\"{}\" number=\"{}\">{}</segment>\n",
                s.bytes,
                s.number,
                esc(&s.message_id)
            ));
        }
        out.push_str("</segments>\n</file>\n");
    }
    out.push_str("</nzb>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, ids: &[(u32, &str)]) -> NzbFile {
        NzbFile {
            name: name.into(),
            segments: ids
                .iter()
                .map(|(n, id)| Segment {
                    number: *n,
                    bytes: 100,
                    message_id: (*id).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn reads_what_it_writes_plain_and_gzipped() {
        let files = vec![
            file(
                "[Group] Show - 01 [ABCD1234].mkv",
                &[(1, "a1@x"), (2, "a2@x")],
            ),
            file("[Group] Show - 01 [ABCD1234].par2", &[(1, "p@x")]),
            file("[Group] Show - 01 [ABCD1234].vol03+04.par2", &[(1, "v@x")]),
        ];
        let xml = write(&files);
        assert_eq!(parse(xml.as_bytes()).unwrap(), files);

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, xml.as_bytes()).unwrap();
        assert_eq!(parse(&gz.finish().unwrap()).unwrap(), files);

        assert!(!files[0].is_par2() && files[1].is_par2());
        let sets: Vec<Option<&str>> = files.iter().map(NzbFile::par2_set).collect();
        assert_eq!(
            sets,
            [
                None,
                Some("[Group] Show - 01 [ABCD1234]"),
                Some("[Group] Show - 01 [ABCD1234]")
            ]
        );
        assert_eq!(
            files
                .iter()
                .map(NzbFile::recovery_blocks)
                .collect::<Vec<_>>(),
            [None, None, Some(4)]
        );
        assert_eq!(files[0].encoded_bytes(), 200);
    }

    #[test]
    fn orders_segments_and_drops_repeats() {
        let xml = write(&[file("a.mkv", &[(2, "b@x"), (1, "a@x"), (2, "b2@x")])]);
        let got = parse(xml.as_bytes()).unwrap();
        assert_eq!(
            got[0].segments.iter().map(|s| s.number).collect::<Vec<_>>(),
            [1, 2]
        );
    }

    /// `NZB_SAMPLE=<a real .nzb>`: what the parser makes of it.
    #[test]
    #[ignore = "needs NZB_SAMPLE"]
    fn reads_a_real_nzb() {
        let Some(path) = std::env::var_os("NZB_SAMPLE") else {
            return;
        };
        let files = parse(&std::fs::read(path).unwrap()).unwrap();
        for f in files.iter().take(12) {
            println!("{:5} segments  {:?}", f.segments.len(), f.name);
        }
        println!("{} files", files.len());
    }

    #[test]
    fn rejects_junk() {
        assert!(parse(b"<html>not an nzb</html>").is_err());
        assert!(parse(&[0x1f, 0x8b, 0, 1]).is_err());
    }
}
