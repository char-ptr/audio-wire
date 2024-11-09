use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait};
use rodio::OutputStream;
use std::fs::File;
use std::io::prelude::*;
use std::sync::mpsc;
use std::thread;
use std::{collections::VecDeque, net::TcpListener};
use std::{error, io};
use tracing::{debug, error, info, trace};
use tracing_subscriber::EnvFilter;
#[cfg(windows)]
use wasapi::*;

#[derive(clap::Parser)]
struct Args {
    #[arg(short, long)]
    address: String,
    #[arg(short, long)]
    port: u16,
    #[arg(short, long)]
    mode: bool,
}
type Res<T> = Result<T, Box<dyn error::Error>>;

// Capture loop, capture samples and send in chunks of "chunksize" frames to channel
#[cfg(windows)]
fn capture_loop(tx_capt: std::sync::mpsc::SyncSender<Vec<u8>>, chunksize: usize) -> Res<()> {
    // Use `Direction::Capture` for normal capture,
    // or `Direction::Render` for loopback mode (for capturing from a playback device).
    let device = get_default_device(&Direction::Render)?;

    let mut audio_client = device.get_iaudioclient()?;

    let desired_format = WaveFormat::new(32, 32, &SampleType::Float, 44100, 2, None);

    let blockalign = desired_format.get_blockalign();
    debug!("Desired capture format: {:?}", desired_format);

    let (def_time, min_time) = audio_client.get_periods()?;
    debug!("default period {}, min period {}", def_time, min_time);

    audio_client.initialize_client(
        &desired_format,
        min_time,
        &Direction::Capture,
        &ShareMode::Shared,
        true,
    )?;
    debug!("initialized capture");

    let h_event = audio_client.set_get_eventhandle()?;

    let buffer_frame_count = audio_client.get_bufferframecount()?;

    let render_client = audio_client.get_audiocaptureclient()?;
    let mut sample_queue: VecDeque<u8> = VecDeque::with_capacity(
        100 * blockalign as usize * (1024 + 2 * buffer_frame_count as usize),
    );
    let session_control = audio_client.get_audiosessioncontrol()?;

    debug!("state before start: {:?}", session_control.get_state());
    audio_client.start_stream()?;
    debug!("state after start: {:?}", session_control.get_state());

    loop {
        while sample_queue.len() > (blockalign as usize * chunksize) {
            debug!("pushing samples");
            let mut chunk = vec![0u8; blockalign as usize * chunksize];
            for element in chunk.iter_mut() {
                *element = sample_queue.pop_front().unwrap();
            }
            tx_capt.send(chunk)?;
        }
        trace!("capturing");
        render_client.read_from_device_to_deque(&mut sample_queue)?;
        if h_event.wait_for_event(3000).is_err() {
            error!("timeout error, stopping capture");
            audio_client.stop_stream()?;
            break;
        }
    }
    Ok(())
}

// Main loop
fn main() -> Res<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    if args.mode {
        #[cfg(windows)]
        {
            initialize_mta().ok()?;

            let (tx_capt, rx_capt): (
                std::sync::mpsc::SyncSender<Vec<u8>>,
                std::sync::mpsc::Receiver<Vec<u8>>,
            ) = mpsc::sync_channel(2);
            let chunksize = 4096;

            // Capture
            let _handle = thread::Builder::new()
                .name("Capture".to_string())
                .spawn(move || {
                    let result = capture_loop(tx_capt, chunksize);
                    if let Err(err) = result {
                        error!("Capture failed with error {}", err);
                    }
                });

            let mut outfile = File::create("recorded.raw")?;

        let addr = format!("{}:{}", args.address, args.port);
        let res = std::net::TcpStream::connect(addr);
        let Ok(mut stream) = res else {
            error!("Could not connect to server");
            return Ok(());
        };
        loop {
            match rx_capt.recv() {
                Ok(chunk) => {
                    debug!("writing to file");

                    stream.write_all(&chunk)?;
                    outfile.write_all(&chunk)?;
                }
                Err(err) => {
                    error!("Some error {}", err);
                    return Ok(());
                }
            }
        };
        Ok(())
    } else {
        error!("Not implemented");
        let server = TcpListener::bind(format!("{}:{}", args.address, args.port))?;
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .expect("no output device available");
        let config = device.default_output_config().unwrap();
        for stream in server.incoming() {
            info!("New connection");
            loop {
                let n = stream.read(&mut buffer)?;
                if n == 0 {
                    info!("breakies");
                    break;
                }
                let data = buffer[..n].to_vec();
                println!("Received data: {:?}", data);
                device.build_output_stream(
                    &config.config(),
                    move |data, _| {
                        io::copy(&mut data, &mut stream).unwrap();
                    },
                    |err| {},
                    None,
                );
            }
        }
        Ok(())
    }
}
