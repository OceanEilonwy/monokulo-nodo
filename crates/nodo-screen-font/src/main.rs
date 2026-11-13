//! nodo-screen-font: builds nodo-screen-16x32 from Terminus ter-v32n when nodo-tools is built.
//!
//! usage: nodo-screen-font ter-v32n.psf.gz nodo-screen-16x32.psf.gz

use std::io::{Read, Write};
use std::process::ExitCode;

fn run(src: &str, dst: &str) -> Result<String, String> {
    let file = std::fs::File::open(src).map_err(|e| format!("{src}: {e}"))?;
    let mut data = Vec::new();
    flate2::read::GzDecoder::new(file).read_to_end(&mut data).map_err(|e| format!("{src}: {e}"))?;
    let (font, added) = nodo_screen_font::make(&data)?;
    // A fixed header (no name, mtime 0) keeps the output the same from build to build.
    let out = std::fs::File::create(dst).map_err(|e| format!("{dst}: {e}"))?;
    let mut gz = flate2::GzBuilder::new().mtime(0).write(out, flate2::Compression::best());
    gz.write_all(&font).and_then(|()| gz.finish().map(drop)).map_err(|e| format!("{dst}: {e}"))?;
    Ok(added)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [src, dst] = args.as_slice() else {
        eprintln!("usage: nodo-screen-font ter-v32n.psf.gz nodo-screen-16x32.psf.gz");
        return ExitCode::from(2);
    };
    match run(src, dst) {
        Ok(added) => {
            println!("added {} glyphs: {added}", added.chars().count());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nodo-screen-font: {e}");
            ExitCode::FAILURE
        }
    }
}
