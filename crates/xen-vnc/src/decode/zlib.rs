/*
Copyright (c) 2016  whitequark <whitequark@whitequark.org>
Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:
The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.
THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
*/

//! A `Read` over one ZRLE payload that feeds a zlib stream kept for the
//! whole connection (RFB ZRLE uses a single zlib stream per connection).
//!
//! Derived from vnc-rs 0.6.1 `src/codec/zlib.rs` (MIT; see `LICENSE-vnc-rs`),
//! which carries the notice above.

use std::io::{Read, Result};

pub(super) struct ZlibReader<'a> {
    decompressor: flate2::Decompress,
    input: &'a [u8],
}

impl<'a> ZlibReader<'a> {
    pub(super) fn new(decompressor: flate2::Decompress, input: &'a [u8]) -> ZlibReader<'a> {
        ZlibReader {
            decompressor,
            input,
        }
    }

    /// Gives the zlib stream back once the payload is used up. A server's
    /// sync flush can leave an empty stored block that produced no output
    /// yet; it is consumed here. Leftover data that would produce pixels is
    /// an error.
    pub(super) fn into_inner(mut self) -> Result<flate2::Decompress> {
        let mut scratch = [0u8; 64];
        while !self.input.is_empty() {
            let in_before = self.decompressor.total_in();
            let out_before = self.decompressor.total_out();
            let status = self.decompressor.decompress(
                self.input,
                &mut scratch,
                flate2::FlushDecompress::Sync,
            );
            let consumed = (self.decompressor.total_in() - in_before) as usize;
            let produced = self.decompressor.total_out() - out_before;
            self.input = &self.input[consumed..];
            if status.is_err() || produced > 0 || consumed == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "leftover zlib byte data",
                ));
            }
        }
        Ok(self.decompressor)
    }

    pub(super) fn read_u8(&mut self) -> std::io::Result<u8> {
        let mut buf = [0; 1];
        self.read_exact(&mut buf)?;
        Ok(buf[0])
    }
}

impl Read for ZlibReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let in_before = self.decompressor.total_in();
        let out_before = self.decompressor.total_out();
        let result =
            self.decompressor
                .decompress(self.input, output, flate2::FlushDecompress::None);
        let consumed = (self.decompressor.total_in() - in_before) as usize;
        let produced = (self.decompressor.total_out() - out_before) as usize;

        self.input = &self.input[consumed..];
        match result {
            Ok(flate2::Status::Ok) => Ok(produced),
            Ok(flate2::Status::BufError) => Ok(0),
            Err(error) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            Ok(flate2::Status::StreamEnd) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "zlib stream end",
            )),
        }
    }
}
