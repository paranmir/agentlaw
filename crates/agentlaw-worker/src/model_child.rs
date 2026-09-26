//! Private model execution boundary. This process never publishes source or index data.
use crate::{EmbeddingError, EmbeddingProvider, ModelAssets, OnnxProvider};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Serialize, Deserialize)]
pub(crate) struct Launch {
    pub incarnation: String,
    pub secret: String,
    pub state: PathBuf,
    pub assets: Option<ModelAssets>,
    pub cached: Option<(String, crate::execution::Selection)>,
}
#[derive(Serialize, Deserialize)]
pub(crate) enum Input {
    Embed { id: i64, attempt: i64, text: String },
    Stop,
}
#[derive(Serialize, Deserialize)]
pub(crate) enum Output {
    Hello {
        incarnation: String,
        control: String,
    },
    Ready {
        model: String,
    },
    Unavailable {
        cause: String,
    },
    Result {
        id: i64,
        attempt: i64,
        result: std::result::Result<Vec<f32>, EmbeddingError>,
    },
    Selection {
        fingerprint: Option<String>,
        selection: crate::execution::Selection,
    },
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Control {
    pub incarnation: String,
    pub secret: String,
    pub stop: bool,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct ControlReply {
    incarnation: String,
    resident_bytes: Option<u64>,
}
// A broker job admits 4 MiB of raw UTF-8. JSON escaping can expand each byte
// to six characters (e.g. NUL -> \u0000), in addition to the envelope.
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(value)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err("model frame exceeds bound".into());
    }
    let mut bytes = Vec::with_capacity(body.len() + 4);
    bytes.extend_from_slice(&u32::try_from(body.len())?.to_be_bytes());
    bytes.extend_from_slice(&body);
    Ok(bytes)
}
pub(crate) fn write<T: Serialize>(w: &mut impl Write, value: &T) -> Result<()> {
    let bytes = encode(value)?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}
pub(crate) fn read<T: serde::de::DeserializeOwned>(r: &mut impl Read) -> Result<T> {
    let mut length = [0; 4];
    r.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err("model frame exceeds bound".into());
    }
    let mut bytes = vec![0; length];
    r.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
/// Hidden `model-child` route. Launch credentials travel only through inherited stdin.
pub fn run_model_child() -> Result<()> {
    let mut input = std::io::stdin().lock();
    let launch: Launch = read(&mut input)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(launch.state.join("model.lock"))?;
    lock.try_lock_exclusive()?; // A surviving incumbent, including orphan grace, prevents double loading.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let output = Arc::new(Mutex::new(std::io::stdout()));
    write(
        &mut *output.lock().unwrap(),
        &Output::Hello {
            incarnation: launch.incarnation.clone(),
            control: listener.local_addr()?.to_string(),
        },
    )?;
    let heartbeat = Arc::new((Mutex::new(Instant::now()), Condvar::new()));
    let clock = heartbeat.clone();
    std::thread::spawn(move || {
        let (time, wake) = &*clock;
        let mut last = time.lock().unwrap();
        loop {
            let remaining = Duration::from_secs(60).saturating_sub(last.elapsed());
            if remaining.is_zero() {
                std::process::exit(0);
            }
            last = wake.wait_timeout(last, remaining).unwrap().0;
        }
    });
    let incarnation = launch.incarnation.clone();
    let secret = launch.secret.clone();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let Ok(message) = read::<Control>(&mut stream) else {
                continue;
            };
            if message.incarnation != incarnation || !constant_equal(&message.secret, &secret) {
                continue;
            }
            *heartbeat.0.lock().unwrap() = Instant::now();
            heartbeat.1.notify_all();
            let _ = write(
                &mut stream,
                &ControlReply {
                    incarnation: incarnation.clone(),
                    resident_bytes: crate::execution::host_resident_bytes(),
                },
            );
            if message.stop {
                std::process::exit(0);
            }
        }
    });
    let provider = match launch
        .assets
        .as_ref()
        .ok_or_else(|| EmbeddingError::Unavailable("explicit model assets not configured".into()))
        .and_then(OnnxProvider::load)
    {
        Ok(p) => Arc::new(p),
        Err(e) => {
            write(
                &mut *output.lock().unwrap(),
                &Output::Unavailable {
                    cause: e.to_string(),
                },
            )?;
            return Ok(());
        }
    };
    write(
        &mut *output.lock().unwrap(),
        &Output::Ready {
            model: provider.model_digest().into(),
        },
    )?;
    if let Some(assets) = launch.assets {
        let provider = provider.clone();
        let output = output.clone();
        std::thread::spawn(move || {
            let fingerprint =
                crate::execution::fingerprint(provider.model_digest(), &assets.runtime_library);
            let cached = launch
                .cached
                .filter(|(key, _)| Some(key) == fingerprint.as_ref())
                .map(|(_, v)| v);
            let selection = provider.select_execution_cached(&assets, cached.as_ref());
            let _ = write(
                &mut *output.lock().unwrap(),
                &Output::Selection {
                    fingerprint,
                    selection,
                },
            );
        });
    }
    loop {
        match read::<Input>(&mut input) {
            Ok(Input::Embed { id, attempt, text }) => {
                let result = provider.embed(&text);
                write(
                    &mut *output.lock().unwrap(),
                    &Output::Result {
                        id,
                        attempt,
                        result,
                    },
                )?;
            }
            Ok(Input::Stop) => break,
            Err(_) => {
                // Lost broker pipe: retain model only for the bounded orphan grace.
                std::thread::park();
            }
        }
    }
    Ok(())
}
fn constant_equal(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |v, (a, b)| v | (a ^ b)) == 0
}
pub(crate) fn control(
    address: &str,
    incarnation: &str,
    secret: &str,
    stop: bool,
) -> Result<Option<u64>> {
    let address: std::net::SocketAddr = address.parse()?;
    if !address.ip().is_loopback() {
        return Err("model control address is not loopback".into());
    }
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write(
        &mut stream,
        &Control {
            incarnation: incarnation.into(),
            secret: secret.into(),
            stop,
        },
    )?;
    let returned: ControlReply = read(&mut stream)?;
    if returned.incarnation != incarnation {
        return Err("model control incarnation mismatch".into());
    }
    Ok(returned.resident_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_admitted_job_survives_json_framing_including_escape_expansion() {
        // The broker admits 4 MiB of UTF-8, not 4 MiB of JSON. This is a transport
        // round trip, not an embedding-quality dataset or a copied encoder.
        for character in ['a', '\0'] {
            let text = character.to_string().repeat(4 * 1024 * 1024);
            let bytes = encode(&Input::Embed {
                id: 1,
                attempt: 1,
                text: text.clone(),
            })
            .expect("broker-admitted input must fit the private transport");
            let Input::Embed { text: actual, .. } = read(&mut &bytes[..]).unwrap() else {
                panic!("input changed its meaning in transit");
            };
            assert_eq!(actual, text);
        }
    }

    #[test]
    fn truncated_and_oversized_frames_are_errors_not_empty_messages() {
        assert!(read::<Input>(&mut &[0, 0, 0][..]).is_err());
        assert!(read::<Input>(&mut &[0, 0, 0, 9, b'{'][..]).is_err());
        // Header alone must reject the allocation, without reading/allocating the body.
        assert!(read::<Input>(&mut &u32::MAX.to_be_bytes()[..]).is_err());
    }
}
