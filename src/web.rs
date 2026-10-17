use core::fmt::Write;

use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_rp::gpio::Input;
use embassy_time::{Duration, Timer};
use embedded_io_async::Write as _;

use crate::motor::StepperMotor;

const PORT: u16 = 80;

/// Serve the control web UI and handle movement commands.
pub async fn run_web_server<'d>(
    stack: Stack<'d>,
    motor: &mut StepperMotor<'d>,
    home_sensor: &Input<'d>,
) -> ! {
    let mut rx_buffer = [0u8; 1024];
    let mut tx_buffer = [0u8; 1024];
    let mut request = [0u8; 256];

    loop {
        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(10)));

        if let Err(_e) = socket.accept(PORT).await {
            continue;
        }
        log::info!("client connected");

        let n = match socket.read(&mut request).await {
            Ok(n) => n,
            Err(_) => continue,
        };

        handle_request(motor, home_sensor, &request[..n]).await;

        let body = render_index(motor);
        let _ = socket.write_all(body.as_bytes()).await;
        let _ = socket.flush().await;

        // Give the browser a moment to read the response before the socket closes.
        Timer::after(Duration::from_millis(50)).await;
    }
}

async fn handle_request<'a>(motor: &mut StepperMotor<'a>, home_sensor: &Input<'a>, request: &[u8]) {
    let path = request_path(request);
    log::info!("request: {}", path);
    match path {
        "/open" => motor.open().await,
        "/close" => motor.close().await,
        "/home" => motor.home(home_sensor).await,
        "/jog-open" => motor.jog(200).await,
        "/jog-close" => motor.jog(-200).await,
        _ => {}
    }
}

fn request_path(request: &[u8]) -> &str {
    // Expect: "GET /path HTTP/1.1"
    let start = match request.windows(4).position(|w| w == b"GET ") {
        Some(i) => i + 4,
        None => return "/",
    };
    let rest = &request[start..];
    let end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
    core::str::from_utf8(&rest[..end]).unwrap_or("/")
}

fn render_index(motor: &StepperMotor<'_>) -> heapless::String<1536> {
    let mut html = heapless::String::<1536>::new();
    let _ = write!(
        html,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Connection: close\r\n\r\n\
         <!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Valve Controller</title><style>\
         body{{font-family:system-ui,sans-serif;text-align:center;background:#16181d;color:#e8e8ea;margin:0;padding:40px}}\
         h1{{font-weight:600}}h2{{color:#7fd1ff}}\
         a{{text-decoration:none}}\
         button{{font-size:18px;padding:14px 26px;margin:8px;cursor:pointer;background:#2f6feb;color:#fff;border:none;border-radius:8px}}\
         button:hover{{background:#1f57c4}}\
         button.alt{{background:#454a55}}button.alt:hover{{background:#33373f}}\
         </style></head><body>\
         <h1>Throttle Valve Controller</h1>\
         <h2>Position: {pct:.1}% ({pos} steps)</h2>\
         <p><a href=\"/open\"><button>Open</button></a>\
         <a href=\"/close\"><button>Close</button></a>\
         <a href=\"/home\"><button class=\"alt\">Home</button></a></p>\
         <p><a href=\"/jog-open\"><button>Jog +200</button></a>\
         <a href=\"/jog-close\"><button>Jog -200</button></a></p>\
         <p><a href=\"/\"><button class=\"alt\">Refresh</button></a></p>\
         </body></html>",
        pct = motor.percent(),
        pos = motor.position(),
    );
    html
}
