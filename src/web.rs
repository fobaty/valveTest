use core::fmt::Write;
use core::sync::atomic::Ordering;

use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_time::{Duration, Timer};
use embedded_io_async::Write as _;

use crate::motor::{self, Command};

const PORT: u16 = 80;

/// Serve the control web UI and dispatch movement commands.
///
/// Commands are only signalled to the motor task here; the actual move happens
/// there, so this connection is answered immediately instead of staying open
/// for the whole travel (which can take seconds).
pub async fn run_web_server(stack: Stack<'_>) -> ! {
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

        let _n = match socket.read(&mut request).await {
            Ok(n) => n,
            Err(_) => continue,
        };

        handle_request(&request[.._n]);

        let body = render_index();
        let _ = socket.write_all(body.as_bytes()).await;
        let _ = socket.flush().await;

        // Give the browser a moment to read the response before the socket closes.
        Timer::after(Duration::from_millis(50)).await;
    }
}

fn handle_request(request: &[u8]) {
    let path = request_path(request);
    log::info!("request: {}", path);

    if path.starts_with("/jog?") {
        if let Some(steps) = parse_query_param(path, "steps") {
            if !motor::EMERGENCY_STOP_ACTIVE.load(Ordering::Relaxed) {
                motor::COMMANDS.signal(Command::Jog(steps));
            }
        }
    } else if path.starts_with("/deg?") {
        if let Some(deg) = parse_query_param(path, "val") {
            let steps_deg_x10 = motor::STEPS_PER_DEGREE_X10.load(Ordering::Relaxed);
            let gear_x10 = motor::GEARBOX_RATIO_X10.load(Ordering::Relaxed);
            let steps = (deg * steps_deg_x10 * gear_x10) / 100;
            if !motor::EMERGENCY_STOP_ACTIVE.load(Ordering::Relaxed) {
                motor::COMMANDS.signal(Command::Jog(steps));
            }
        }
    } else if path.starts_with("/config?") {
        if let Some(max) = parse_query_param(path, "max") {
            let max = max.clamp(100, 1_000_000);
            motor::MAX_TRAVEL_STEPS.store(max, Ordering::Relaxed);
            let auto_deg_x10 = (max * 10) / 90;
            motor::STEPS_PER_DEGREE_X10.store(auto_deg_x10, Ordering::Relaxed);
        }
        if let Some(deg_x10) = parse_query_param(path, "deg_x10") {
            motor::STEPS_PER_DEGREE_X10.store(deg_x10.clamp(1, 10000), Ordering::Relaxed);
        }
        if let Some(gear_x10) = parse_query_param(path, "gear_x10") {
            motor::GEARBOX_RATIO_X10.store(gear_x10.clamp(1, 1000), Ordering::Relaxed);
        }
        let en = parse_query_param(path, "en").unwrap_or(0) == 1;
        motor::MAX_STEPS_ENABLED.store(en, Ordering::Relaxed);
    } else {
        let command = match path {
            "/open" => Some(Command::Open),
            "/close" => Some(Command::Close),
            "/home" => Some(Command::Home),
            "/jog-open" => Some(Command::Jog(200)),
            "/jog-close" => Some(Command::Jog(-200)),
            "/estop" => {
                motor::EMERGENCY_STOP_ACTIVE.store(true, Ordering::Relaxed);
                None
            }
            "/resume-estop" => {
                motor::EMERGENCY_STOP_ACTIVE.store(false, Ordering::Relaxed);
                None
            }
            _ => None,
        };
        if let Some(command) = command {
            if !motor::EMERGENCY_STOP_ACTIVE.load(Ordering::Relaxed) {
                motor::COMMANDS.signal(command);
            } else {
                log::warn!("Command ignored: emergency stop is active");
            }
        }
    }
}

fn parse_query_param(path: &str, key: &str) -> Option<i32> {
    let query_start = path.find('?')?;
    let query = &path[query_start + 1..];
    for pair in query.split('&') {
        let mut kv = pair.split('=');
        if kv.next() == Some(key) {
            let val_str = kv.next()?;
            return val_str.parse::<i32>().ok();
        }
    }
    None
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

fn render_index() -> heapless::String<8192> {
    let position = motor::POSITION.load(Ordering::Relaxed);
    let busy = motor::BUSY.load(Ordering::Relaxed);
    let estop_active = motor::EMERGENCY_STOP_ACTIVE.load(Ordering::Relaxed);
    let max_steps = motor::MAX_TRAVEL_STEPS.load(Ordering::Relaxed);
    let max_enabled = motor::MAX_STEPS_ENABLED.load(Ordering::Relaxed);
    let steps_deg_x10 = motor::STEPS_PER_DEGREE_X10.load(Ordering::Relaxed);
    let gear_x10 = motor::GEARBOX_RATIO_X10.load(Ordering::Relaxed);

    let max_enabled_checked = if max_enabled { "checked" } else { "" };

    let percent = if max_steps > 0 {
        (position as f32 / max_steps as f32 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let degrees = if steps_deg_x10 > 0 && gear_x10 > 0 {
        (position as i64 * 100) as f32 / (steps_deg_x10 * gear_x10) as f32
    } else {
        0.0
    };
    let status = if busy { " — moving" } else { "" };

    // Auto-refresh when moving so position updates
    let refresh = if busy {
        "<meta http-equiv=\"refresh\" content=\"1\">"
    } else {
        ""
    };

    let estop_section = if estop_active {
        "<div style=\"background:#ff5555;color:#000;padding:16px;border-radius:10px;margin-bottom:20px;font-weight:bold\">\
         🛑 EMERGENCY STOP ACTIVE 🛑<br>\
         <p style=\"margin:10px 0 0 0\"><a href=\"/resume-estop\"><button style=\"background:#000;color:#fff;font-size:16px;padding:10px 20px\">Resume / Clear E-Stop</button></a></p>\
         </div>"
    } else {
        "<div style=\"margin-bottom:20px\">\
         <a href=\"/estop\"><button style=\"background:#ff5555;color:#000;font-size:18px;padding:14px 28px;font-weight:bold\">🔴 EMERGENCY STOP</button></a>\
         </div>"
    };

    let mut html = heapless::String::<8192>::new();
    let _ = write!(
        html,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Connection: close\r\n\r\n\
         <!DOCTYPE html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         {refresh}\
         <title>Valve Controller</title><style>\
         body{{font-family:system-ui,sans-serif;text-align:center;background:#16181d;color:#e8e8ea;margin:0;padding:20px}}\
         h1{{font-weight:600}}h2{{color:#7fd1ff}}\
         a{{text-decoration:none}}\
         button{{font-size:16px;padding:12px 20px;margin:6px;cursor:pointer;background:#2f6feb;color:#fff;border:none;border-radius:8px}}\
         button:hover{{background:#1f57c4}}\
         button.alt{{background:#454a55}}button.alt:hover{{background:#33373f}}\
         .card{{background:#21252b;padding:15px;border-radius:10px;margin:15px auto;max-width:600px;text-align:left}}\
         .card h3{{margin-top:0;color:#7fd1ff;text-align:center}}\
         table{{width:100%%;border-collapse:collapse;font-size:14px}}\
         th,td{{padding:6px 8px;border-bottom:1px solid #333;text-align:left}}\
         input{{padding:8px;margin:4px;border-radius:6px;border:1px solid #444;background:#16181d;color:#fff}}\
         </style></head><body>\
         <h1>Throttle Valve Controller <span style=\"font-size:14px;color:#7fd1ff\">{version}</span></h1>\
         {estop_section}\
         <h2>Position: {pct:.1}% ({deg:.1}°, {pos} steps){status}</h2>\
         <div class=\"card\">\
         <h3>Control</h3>\
         <div style=\"text-align:center\">\
         <a href=\"/open\"><button>Open (+90°)</button></a>\
         <a href=\"/close\"><button>Close (-90°)</button></a>\
         <a href=\"/home\"><button class=\"alt\">Home (Find Zero / Close Limit)</button></a>\
         </div>\
         <div style=\"text-align:center;margin-top:10px\">\
         <a href=\"/deg?val=10\"><button>+10°</button></a>\
         <a href=\"/deg?val=-10\"><button>-10°</button></a>\
         <a href=\"/deg?val=45\"><button>+45°</button></a>\
         <a href=\"/deg?val=-45\"><button>-45°</button></a>\
         <a href=\"/deg?val=90\"><button>+90°</button></a>\
         <a href=\"/deg?val=-90\"><button>-90°</button></a>\
         <a href=\"/deg?val=180\"><button>+180°</button></a>\
         <a href=\"/deg?val=-180\"><button>-180°</button></a>\
         </div>\
         </div>\
         <div class=\"card\">\
         <h3>Motor & Travel Settings</h3>\
         <form action=\"/config\" method=\"get\">\
         <label><input type=\"checkbox\" name=\"en\" value=\"1\" {max_enabled_checked}> Enable Max Steps Software Lock</label><br><br>\
         Max Steps (Open to Close): <input type=\"number\" name=\"max\" value=\"{max_steps}\" style=\"width:100px\"><br>\
         Steps/Deg (x10): <input type=\"number\" name=\"deg_x10\" value=\"{steps_deg_x10}\" style=\"width:80px\"><br>\
         Gearbox Ratio (x10): <input type=\"number\" name=\"gear_x10\" value=\"{gear_x10}\" style=\"width:80px\"> (e.g. 10=1.0x, 100=10.0x)\
         <br><button type=\"submit\" style=\"padding:8px 14px;margin-top:10px\">Save Settings</button>\
         </form>\
         </div>\
         <div class=\"card\">\
         <h3>Wiring & Pinout Reference (Pico 2W)</h3>\
         <table>\
         <tr><th>Component</th><th>Pico GPIO</th><th>Connection</th></tr>\
         <tr><td>STEP (Driver CLK)</td><td>GP2</td><td>-CW (CLK)</td></tr>\
         <tr><td>DIR (Driver UD)</td><td>GP3</td><td>-CCW (UD)</td></tr>\
         <tr><td>ENABLE</td><td>GP4</td><td>Driver Enable</td></tr>\
         <tr><td>Limit Switch (Close / Home)</td><td>GP5</td><td>To GND (internal pull-up)</td></tr>\
         <tr><td>Limitswitch Indicator LED</td><td>GP6</td><td>LED + Resistor to GND</td></tr>\
         <tr><td>Open Active Indicator LED</td><td>GP7</td><td>LED + Resistor to GND</td></tr>\
         <tr><td>Close Active Indicator LED</td><td>GP8</td><td>LED + Resistor to GND</td></tr>\
         <tr><td>Open Button</td><td>GP9</td><td>Momentary switch to GND</td></tr>\
         <tr><td>Close Button</td><td>GP10</td><td>Momentary switch to GND</td></tr>\
         <tr><td>Home Button</td><td>GP11</td><td>Momentary switch to GND</td></tr>\
         <tr><td>Jog+ (Open +) Button</td><td>GP12</td><td>Momentary switch to GND</td></tr>\
         <tr><td>Jog- (Close -) Button</td><td>GP13</td><td>Momentary switch to GND</td></tr>\
         <tr><td>Emergency Stop Button</td><td>GP14</td><td>Momentary switch to GND</td></tr>\
         </table>\
         <h3 style=\"margin-top:15px\">Physical Buttons Description</h3>\
         <table>\
         <tr><th>Button</th><th>GPIO</th><th>Action</th></tr>\
         <tr><td>Open</td><td>GP9</td><td>Rotates valve +90 degrees from current position (opening direction)</td></tr>\
         <tr><td>Close</td><td>GP10</td><td>Rotates valve -90 degrees from current position (closing direction); stops early if Close limit switch triggers</td></tr>\
         <tr><td>Home</td><td>GP11</td><td>Searches zero position: backs off limit switch, then closes until Close limit switch triggers, resets position to 0</td></tr>\
         <tr><td>Jog+</td><td>GP12</td><td>Short opening jog (+200 steps) for fine adjustment</td></tr>\
         <tr><td>Jog-</td><td>GP13</td><td>Short closing jog (-200 steps) for fine adjustment</td></tr>\
         <tr><td>Emergency Stop</td><td>GP14</td><td>Immediately stops all movement and locks motor; clear via web E-Stop resume button</td></tr>\
         </table>\
         <p style=\"font-size:13px;color:#999\">All buttons are momentary switches wired between the GPIO pin and GND (internal pull-up enabled). \
         Note: physical buttons are currently disabled in firmware until the pins are wired to GND to prevent floating-pin false triggers.</p>\
         </div>\
         <p><a href=\"/\"><button class=\"alt\">Refresh</button></a></p>\
         </body></html>",
        version = motor::FIRMWARE_VERSION,
        pct = percent,
        deg = degrees,
        pos = position,
        status = status,
        max_steps = max_steps,
        max_enabled_checked = max_enabled_checked,
        steps_deg_x10 = steps_deg_x10,
        gear_x10 = gear_x10,
        estop_section = estop_section,
    );
    html
}
