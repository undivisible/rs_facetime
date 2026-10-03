//! Generated samples only: no device, microphone, permission, or FaceTime access.
use rs_facetime::audio::{pcm_channel, BufferConfig, FrameTimestamp, PcmFormat, PcmFrame};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let format = PcmFormat::new(48_000, 1)?;
    let (sender, mut stream) = pcm_channel(format, BufferConfig::new(2, 480)?)?;
    let samples = (0..480)
        .map(|n| (n as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.1)
        .collect();
    sender.try_send(PcmFrame::new(
        format,
        samples,
        FrameTimestamp {
            sequence: 0,
            host_time: None,
        },
    )?)?;
    sender.finish(None);
    if let Some(frame) = stream.try_next()? {
        // Processing belongs to the caller. Samples are never written to disk.
        let mean_square = frame
            .samples()
            .iter()
            .map(|sample| sample * sample)
            .sum::<f32>()
            / frame.samples().len() as f32;
        println!(
            "{} generated frames, RMS {:.4}",
            frame.frame_count(),
            mean_square.sqrt()
        );
    }
    Ok(())
}
