use crate::{Decoder, Demuxer, DemuxerInfo, Encoder, Muxer, Scaler, StreamInfoChannel};
use anyhow::Result;
use ffmpeg_sys_the_third::{av_frame_free, av_packet_free};
use std::collections::HashMap;
use std::path::PathBuf;
use std::ptr;

/// A common transcoder task taking an input file
/// and transcoding it to another output path
pub struct Transcoder {
    demuxer: Demuxer,
    decoder: Decoder,
    scalers: HashMap<i32, Scaler>,
    encoders: HashMap<i32, Encoder>,
    copy_stream: HashMap<i32, i32>,
    muxer: Muxer,
}

impl Transcoder {
    pub unsafe fn new(input: &str, output: &str) -> Result<Self> {
        let muxer = Muxer::new().with_output(&PathBuf::from(output), None, None)?;

        Ok(Self {
            demuxer: Demuxer::new(input)?,
            decoder: Decoder::new(),
            scalers: HashMap::new(),
            encoders: HashMap::new(),
            copy_stream: HashMap::new(),
            muxer,
        })
    }

    /// Prepare the transcoder by probing the input
    pub unsafe fn prepare(&mut self) -> Result<DemuxerInfo> {
        self.demuxer.probe_input()
    }

    /// Create a transcoded stream in the output given an input stream and
    /// a pre-configured output encoder
    pub unsafe fn transcode_stream(
        &mut self,
        in_stream: &StreamInfoChannel,
        encoder_out: Encoder,
    ) -> Result<()> {
        let src_index = in_stream.index as i32;
        let dst_stream = self.muxer.add_stream_encoder(&encoder_out)?;
        let out_ctx = encoder_out.codec_context();
        if in_stream.width != (*out_ctx).width as usize
            || in_stream.height != (*out_ctx).height as usize
            || in_stream.format != (*out_ctx).pix_fmt as usize {
            // Setup scaler if the size/format is different from what the codec expects
            self.scalers.insert(src_index, Scaler::new());
        }
        self.encoders.insert(
            src_index,
            encoder_out.with_stream_index((*dst_stream).index),
        );
        self.decoder.setup_decoder(in_stream, None)?;
        Ok(())
    }

    /// Copy a stream from the input to the output
    pub unsafe fn copy_stream(&mut self, in_stream: StreamInfoChannel) -> Result<()> {
        let dst_stream = self.muxer.add_copy_stream(in_stream.stream)?;
        self.copy_stream
            .insert(in_stream.index as i32, (*dst_stream).index);
        Ok(())
    }

    /// Process the next packet, called by [run]
    unsafe fn next(&mut self) -> Result<bool> {
        let (mut pkt, stream) = self.demuxer.get_packet()?;

        // flush
        if pkt.is_null() {
            for (_, enc) in &mut self.encoders {
                for mut new_pkt in enc.encode_frame(ptr::null_mut())? {
                    self.muxer.write_packet(new_pkt)?;
                    av_packet_free(&mut new_pkt);
                }
            }
            Ok(true)
        } else {
            let src_index = (*stream).index;
            // check if encoded stream
            if let Some(enc) = self.encoders.get_mut(&src_index) {
                for mut frame in self.decoder.decode_pkt(pkt, stream)? {

                    // scale frame before sending to encoder
                    let mut frame = if let Some(mut sws) = self.scalers.get_mut(&src_index) {
                        let enc_ctx = enc.codec_context();
                        let new_frame = sws.process_frame(frame, (*enc_ctx).width as u16, (*enc_ctx).height as u16, (*enc_ctx).pix_fmt)?;
                        av_frame_free(&mut frame);
                        new_frame
                    } else {
                        frame
                    };

                    // encode frame and send packets to muxer
                    for mut new_pkt in enc.encode_frame(frame)? {
                        self.muxer.write_packet(new_pkt)?;
                        av_packet_free(&mut new_pkt);
                    }
                    av_frame_free(&mut frame);
                }
            } else if let Some(dst_stream) = self.copy_stream.get(&src_index) {
                // write pkt directly to muxer (re-mux)
                (*pkt).stream_index = *dst_stream;
                self.muxer.write_packet(pkt)?;
            }

            av_packet_free(&mut pkt);
            Ok(false)
        }
    }

    /// Run the transcoder
    pub unsafe fn run(mut self) -> Result<()> {
        self.muxer.open()?;
        while !self.next()? {
            // nothing here
        }
        self.muxer.close()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_remux() -> Result<()> {
        unsafe {
            std::fs::create_dir_all("test_output")?;
            let mut transcoder =
                Transcoder::new("test_output/test.mp4", "test_output/test_transcode.mkv")?;
            let info = transcoder.prepare()?;
            for c in info.channels {
                transcoder.copy_stream(c)?;
            }
            transcoder.run()?;

            Ok(())
        }
    }
}
