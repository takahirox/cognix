#![allow(dead_code)]
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}
impl Reply {
    pub fn json(v: Value) -> Self {
        Self {
            status: 200,
            body: v.to_string(),
            delay: Duration::ZERO,
        }
    }
}
pub struct Server {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    pub fn new(handler: impl Fn(&str, &Value) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let records = requests.clone();
        let worker = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                if let Some((path, body)) = read_request(&mut stream) {
                    records
                        .lock()
                        .unwrap()
                        .push(json!({"path":path,"body":body}));
                    let reply = handler(&path, &body);
                    std::thread::sleep(reply.delay);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.status,
                        reply.body.len(),
                        reply.body
                    );
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn read_request(stream: &mut TcpStream) -> Option<(String, Value)> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let end;
    loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            end = i + 4;
            break;
        }
    }
    let header = String::from_utf8_lossy(&raw[..end]);
    let path = header.split_whitespace().nth(1)?.to_owned();
    let length = header
        .lines()
        .find_map(|l| {
            l.to_lowercase()
                .strip_prefix("content-length:")
                .and_then(|s| s.trim().parse::<usize>().ok())
        })
        .unwrap_or(0);
    while raw.len() < end + length {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
    }
    Some((
        path,
        serde_json::from_slice(&raw[end..end + length]).unwrap_or(Value::Null),
    ))
}
pub fn ready(path: &str, _: &Value) -> Reply {
    Reply::json(match path {
        "/api/version" => json!({"version":"fixture-1"}),
        "/api/show" => {
            json!({"capabilities":["completion","tools"],"model_info":{"fixture.context_length":32768}})
        }
        _ => {
            json!({"done":true,"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"cognix_probe","arguments":{"value":"ok"}}}]}})
        }
    })
}
