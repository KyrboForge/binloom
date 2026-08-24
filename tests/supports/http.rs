use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};

pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub path: String,
    pub authorization: Option<String>,
}

pub struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    pub fn start(responses: Vec<Response>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);

        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];

                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }

                    bytes.extend_from_slice(&buffer[..count]);

                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }

                let request = String::from_utf8_lossy(&bytes);
                let mut lines = request.lines();
                let path = lines
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap()
                    .to_owned();

                let authorization = lines.find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("authorization")
                        .then(|| value.trim().to_owned())
                });

                captured.lock().unwrap().push(Request {
                    path,
                    authorization,
                });

                let reason = match response.status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Error",
                };

                write!(
                    stream,
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status,
                    reason,
                    response.body.len()
                )
                .unwrap();

                stream.write_all(&response.body).unwrap();
                stream.flush().unwrap();
            }
        });

        Self {
            url: format!("http://{address}"),
            requests,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
