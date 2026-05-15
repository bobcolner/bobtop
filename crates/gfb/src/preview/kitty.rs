//! Direct kitty graphics protocol emitter.
//!
//! Bypasses viuer's runtime kitty-support detection (which queries the
//! terminal and times out over SSH or inside tmux, then silently falls
//! back to text blocks) by writing the kitty graphics escape sequences
//! ourselves. Uses *direct data transmission* (`t=d`) so the bytes
//! travel inline through the SSH stream rather than as a file path the
//! local terminal can't open.
//!
//! Protocol reference: https://sw.kovidgoyal.net/kitty/graphics-protocol/
//!
//! Single-image flow:
//!   1. Decode the file (or accept a `DynamicImage`) and re-encode as
//!      PNG bytes — kitty's `f=100` format.
//!   2. Base64-encode and split into ≤4096-byte chunks (the protocol's
//!      hard upper bound per escape).
//!   3. Position the cursor (CSI H), then emit chunks: first carries
//!      the placement parameters (`a=T,f=100,c=…,r=…`), each non-final
//!      chunk has `m=1`, the final chunk has `m=0`.

use std::io::{self, Cursor, Write};
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use image::ImageFormat;

const CHUNK: usize = 4096;

/// Paint `path` as a kitty-protocol image at terminal cell `(x, y)`,
/// sized to display in `cols × rows` cells. The image is re-encoded as
/// PNG in memory so any of our supported decode formats (JPG/PNG/GIF/
/// WebP) round-trip cleanly. Errors are returned to the caller; the
/// run loop drops them silently rather than panic.
pub fn paint_image_at(path: &Path, x: u16, y: u16, cols: u16, rows: u16) -> io::Result<()> {
    let img = image::open(path)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("decode: {e}")))?;
    let mut png_bytes: Vec<u8> = Vec::with_capacity(64 * 1024);
    img.write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("encode png: {e}")))?;
    paint_png_bytes_at(&png_bytes, x, y, cols, rows)
}

/// Lower-level entry: emit pre-encoded PNG bytes. Splits into kitty
/// protocol chunks, prefixes the first with placement parameters, and
/// flushes once at the end so partial sequences don't leak between
/// frames.
pub fn paint_png_bytes_at(png: &[u8], x: u16, y: u16, cols: u16, rows: u16) -> io::Result<()> {
    let encoded = STANDARD.encode(png);
    let mut stdout = io::stdout().lock();
    // Move the cursor to (x, y). CSI uses 1-based coordinates; the
    // image is anchored at the top-left of this cell and grows down
    // and right by `rows`/`cols` cells.
    write!(stdout, "\x1b[{};{}H", y.saturating_add(1), x.saturating_add(1))?;

    let total = encoded.len();
    let mut start = 0;
    let mut first = true;
    while start < total {
        let end = (start + CHUNK).min(total);
        let chunk = &encoded[start..end];
        let more = if end < total { 1 } else { 0 };
        if first {
            // a=T transmit-and-display; f=100 PNG; c/r display cell
            // dimensions; q=2 silences error responses (the run loop
            // ignores them anyway, and unhandled responses can confuse
            // ratatui's input parsing).
            write!(
                stdout,
                "\x1b_Ga=T,f=100,c={cols},r={rows},q=2,m={more};"
            )?;
            first = false;
        } else {
            write!(stdout, "\x1b_Gm={more},q=2;")?;
        }
        stdout.write_all(chunk.as_bytes())?;
        write!(stdout, "\x1b\\")?;
        start = end;
    }
    stdout.flush()
}

/// Erase any kitty image currently placed on the terminal. Sent on
/// preview transitions so a stale image doesn't linger when the user
/// moves to a non-image entry.
pub fn clear_all_images() -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    // a=d (delete) with d=A (all placements). q=2 silences the
    // protocol's response.
    write!(stdout, "\x1b_Ga=d,d=A,q=2;\x1b\\")?;
    stdout.flush()
}
