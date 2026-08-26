use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) path: String,
    pub(crate) authorization: Option<String>,
    pub(crate) private_token: Option<String>,
}

pub(crate) struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    pub(crate) fn start(responses: Vec<Response>) -> Self {
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

                let mut authorization = None;
                let mut private_token = None;

                for line in lines {
                    let Some((name, value)) = line.split_once(':') else {
                        continue;
                    };

                    if name.eq_ignore_ascii_case("authorization") {
                        authorization = Some(value.trim().to_owned());
                    }

                    if name.eq_ignore_ascii_case("private-token") {
                        private_token = Some(value.trim().to_owned());
                    }
                }

                captured.lock().unwrap().push(Request {
                    path,
                    authorization,
                    private_token,
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

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}
