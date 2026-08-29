use anyhow::{bail, Context};
use clap::{Parser, Subcommand};
use cua_capture::encode_image;
use cua_core::{CursorState, FrameEncoding, WindowInfo};
use image::{ImageBuffer, Rgba};
use std::io::{Read, Write};
use std::time::Duration;
#[cfg(target_os = "linux")]
use x11_clipboard::Clipboard;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as XprotoExt, ImageFormat, ImageOrder, Window,
};
use x11rb::protocol::xtest::ConnectionExt as XtestExt;
use x11rb::rust_connection::RustConnection;

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;
const MOTION_NOTIFY: u8 = 6;

#[derive(Debug, Parser)]
#[command(
    name = "cua-qgui-tool",
    about = "Bundled X11 tool for qgui-backed CUA nodes"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    CapturePng,
    CursorJson,
    WindowsJson,
    MouseMove {
        x: i16,
        y: i16,
        duration_ms: u64,
    },
    MouseClick {
        x: i16,
        y: i16,
        button: String,
        count: u8,
    },
    MouseDrag {
        from_x: i16,
        from_y: i16,
        to_x: i16,
        to_y: i16,
        duration_ms: u64,
    },
    Key {
        combo: String,
    },
    Type {
        text: String,
    },
    ClipboardRead,
    ClipboardWrite,
    ClipboardServe,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let (conn, screen_num) = x11rb::connect(None).context("connect to qgui X11 display")?;
    let root = conn.setup().roots[screen_num].root;
    match cli.command {
        Cmd::CapturePng => capture_png(&conn, root)?,
        Cmd::CursorJson => cursor_json(&conn, root)?,
        Cmd::WindowsJson => windows_json(&conn, root)?,
        Cmd::MouseMove { x, y, duration_ms } => {
            motion(&conn, root, x, y)?;
            sleep_ms(duration_ms);
        }
        Cmd::MouseClick {
            x,
            y,
            button,
            count,
        } => {
            motion(&conn, root, x, y)?;
            let button = parse_button(&button)?;
            for _ in 0..count.max(1) {
                send_xtest_input(&conn, BUTTON_PRESS, button, root, x, y)?;
                send_xtest_input(&conn, BUTTON_RELEASE, button, root, x, y)?;
            }
        }
        Cmd::MouseDrag {
            from_x,
            from_y,
            to_x,
            to_y,
            duration_ms,
        } => {
            motion(&conn, root, from_x, from_y)?;
            send_xtest_input(&conn, BUTTON_PRESS, 1, root, from_x, from_y)?;
            motion(&conn, root, to_x, to_y)?;
            sleep_ms(duration_ms);
            send_xtest_input(&conn, BUTTON_RELEASE, 1, root, to_x, to_y)?;
        }
        Cmd::Key { combo } => send_combo(&conn, root, &combo)?,
        Cmd::Type { text } => type_text(&conn, root, &text)?,
        Cmd::ClipboardRead => clipboard_read()?,
        Cmd::ClipboardWrite => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("read clipboard text from stdin")?;
            clipboard_write(text)?
        }
        Cmd::ClipboardServe => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("read clipboard text from stdin")?;
            clipboard_serve(text)?
        }
    }
    conn.flush().context("flush X11 commands")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn clipboard_read() -> anyhow::Result<()> {
    let clipboard = Clipboard::new().context("connect to X11 clipboard")?;
    let bytes = clipboard
        .load(
            clipboard.setter.atoms.clipboard,
            clipboard.setter.atoms.utf8_string,
            clipboard.setter.atoms.property,
            Duration::from_secs(3),
        )
        .context("read X11 clipboard")?;
    std::io::stdout()
        .write_all(&bytes)
        .context("write clipboard text to stdout")?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn clipboard_read() -> anyhow::Result<()> {
    bail!("native qgui clipboard read is only implemented for Linux")
}

#[cfg(target_os = "linux")]
fn clipboard_write(text: String) -> anyhow::Result<()> {
    let clipboard = Clipboard::new().context("connect to X11 clipboard")?;
    store_clipboard(&clipboard, text)
}

#[cfg(target_os = "linux")]
fn clipboard_serve(text: String) -> anyhow::Result<()> {
    let clipboard = Clipboard::new().context("connect to X11 clipboard")?;
    store_clipboard(&clipboard, text)?;
    println!("ready");
    loop {
        std::thread::park();
    }
}

#[cfg(target_os = "linux")]
fn store_clipboard(clipboard: &Clipboard, text: String) -> anyhow::Result<()> {
    clipboard
        .store(
            clipboard.setter.atoms.clipboard,
            clipboard.setter.atoms.utf8_string,
            text.into_bytes(),
        )
        .context("write X11 clipboard")?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn clipboard_write(_text: String) -> anyhow::Result<()> {
    bail!("native qgui clipboard write is only implemented for Linux")
}

#[cfg(not(target_os = "linux"))]
fn clipboard_serve(_text: String) -> anyhow::Result<()> {
    bail!("native qgui clipboard owner is only implemented for Linux")
}

fn capture_png(conn: &RustConnection, root: Window) -> anyhow::Result<()> {
    let setup = conn.setup();
    let screen = setup
        .roots
        .iter()
        .find(|screen| screen.root == root)
        .context("find root screen")?;
    let image = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            root,
            0,
            0,
            screen.width_in_pixels,
            screen.height_in_pixels,
            u32::MAX,
        )
        .context("request X11 root image")?
        .reply()
        .context("read X11 root image")?;
    let format = setup
        .pixmap_formats
        .iter()
        .find(|format| format.depth == image.depth)
        .context("find X11 pixmap format")?;
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|depth| depth.visuals.iter())
        .find(|visual| visual.visual_id == screen.root_visual)
        .context("find X11 root visual")?;
    let rgba = decode_true_color(X11ImageLayout {
        data: &image.data,
        width: screen.width_in_pixels,
        height: screen.height_in_pixels,
        bits_per_pixel: format.bits_per_pixel,
        byte_order: setup.image_byte_order,
        red_mask: visual.red_mask,
        green_mask: visual.green_mask,
        blue_mask: visual.blue_mask,
    })?;
    let bytes = encode_image(&rgba, FrameEncoding::Png).context("encode X11 capture png")?;
    std::io::stdout()
        .write_all(&bytes)
        .context("write png to stdout")?;
    Ok(())
}

struct X11ImageLayout<'a> {
    data: &'a [u8],
    width: u16,
    height: u16,
    bits_per_pixel: u8,
    byte_order: ImageOrder,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
}

fn decode_true_color(layout: X11ImageLayout<'_>) -> anyhow::Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let bytes_per_pixel = usize::from(layout.bits_per_pixel / 8);
    if bytes_per_pixel == 0 {
        bail!("unsupported X11 bits_per_pixel={}", layout.bits_per_pixel);
    }
    let mut out = ImageBuffer::new(u32::from(layout.width), u32::from(layout.height));
    for y in 0..layout.height {
        for x in 0..layout.width {
            let offset =
                (usize::from(y) * usize::from(layout.width) + usize::from(x)) * bytes_per_pixel;
            let pixel = read_pixel(
                layout
                    .data
                    .get(offset..offset + bytes_per_pixel)
                    .context("X11 image data ended early")?,
                layout.byte_order,
            );
            out.put_pixel(
                u32::from(x),
                u32::from(y),
                Rgba([
                    extract_channel(pixel, layout.red_mask),
                    extract_channel(pixel, layout.green_mask),
                    extract_channel(pixel, layout.blue_mask),
                    255,
                ]),
            );
        }
    }
    Ok(out)
}

fn read_pixel(bytes: &[u8], byte_order: ImageOrder) -> u32 {
    let mut padded = [0u8; 4];
    padded[..bytes.len().min(4)].copy_from_slice(&bytes[..bytes.len().min(4)]);
    match byte_order {
        ImageOrder::LSB_FIRST => u32::from_le_bytes(padded),
        ImageOrder::MSB_FIRST => u32::from_be_bytes(padded),
        _ => u32::from_ne_bytes(padded),
    }
}

fn extract_channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = mask.count_ones();
    let raw = (pixel & mask) >> shift;
    let max = (1u32 << bits) - 1;
    ((raw * 255) / max) as u8
}

fn cursor_json(conn: &RustConnection, root: Window) -> anyhow::Result<()> {
    let pointer = conn
        .query_pointer(root)
        .context("query X11 pointer")?
        .reply()
        .context("read X11 pointer")?;
    let cursor = CursorState {
        x: f64::from(pointer.root_x),
        y: f64::from(pointer.root_y),
        visible: true,
        included_in_frame: false,
    };
    println!("{}", serde_json::to_string(&cursor)?);
    Ok(())
}

fn windows_json(conn: &RustConnection, root: Window) -> anyhow::Result<()> {
    let tree = conn
        .query_tree(root)
        .context("query X11 window tree")?
        .reply()
        .context("read X11 window tree")?;
    let active = active_window(conn, root).unwrap_or_default();
    let mut windows = Vec::new();
    for window in tree.children {
        let Ok(cookie) = conn.get_geometry(window) else {
            continue;
        };
        let Ok(geometry) = cookie.reply() else {
            continue;
        };
        if geometry.width == 0 || geometry.height == 0 {
            continue;
        }
        windows.push(WindowInfo {
            id: format!("0x{window:08x}"),
            app_name: None,
            title: window_title(conn, window)
                .ok()
                .filter(|title| !title.is_empty()),
            layer: 0,
            x: i32::from(geometry.x),
            y: i32::from(geometry.y),
            width: u32::from(geometry.width),
            height: u32::from(geometry.height),
            focused: Some(window) == active,
        });
    }
    println!("{}", serde_json::to_string(&windows)?);
    Ok(())
}

fn active_window(conn: &RustConnection, root: Window) -> anyhow::Result<Option<Window>> {
    let atom = intern(conn, "_NET_ACTIVE_WINDOW")?;
    let reply = conn
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)
        .context("request _NET_ACTIVE_WINDOW")?
        .reply()
        .context("read _NET_ACTIVE_WINDOW")?;
    Ok(reply.value32().and_then(|mut values| values.next()))
}

fn window_title(conn: &RustConnection, window: Window) -> anyhow::Result<String> {
    let utf8 = intern(conn, "UTF8_STRING")?;
    let title_atom = intern(conn, "_NET_WM_NAME")?;
    let reply = conn
        .get_property(false, window, title_atom, utf8, 0, 1024)
        .context("request _NET_WM_NAME")?
        .reply()
        .context("read _NET_WM_NAME")?;
    if !reply.value.is_empty() {
        return Ok(String::from_utf8_lossy(&reply.value).to_string());
    }
    let reply = conn
        .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 1024)
        .context("request WM_NAME")?
        .reply()
        .context("read WM_NAME")?;
    Ok(String::from_utf8_lossy(&reply.value).to_string())
}

fn intern(conn: &RustConnection, name: &str) -> anyhow::Result<u32> {
    Ok(conn
        .intern_atom(false, name.as_bytes())
        .with_context(|| format!("intern X11 atom {name}"))?
        .reply()
        .with_context(|| format!("read X11 atom {name}"))?
        .atom)
}

fn send_combo(conn: &RustConnection, root: Window, combo: &str) -> anyhow::Result<()> {
    let keyboard = KeyboardMap::load(conn)?;
    let mut pressed = Vec::new();
    for part in combo.split('+') {
        let key = part.trim();
        let keysym = named_keysym(key).or_else(|| key.chars().next().map(|ch| ch as u32));
        let keysym = keysym.with_context(|| format!("unsupported key combo part '{key}'"))?;
        let keycode = keyboard
            .keycode_for(keysym)
            .with_context(|| format!("keysym 0x{keysym:x} is not present in the X11 keymap"))?;
        send_xtest_input(conn, KEY_PRESS, keycode, root, 0, 0)?;
        pressed.push(keycode);
    }
    for keycode in pressed.into_iter().rev() {
        send_xtest_input(conn, KEY_RELEASE, keycode, root, 0, 0)?;
    }
    Ok(())
}

fn type_text(conn: &RustConnection, root: Window, text: &str) -> anyhow::Result<()> {
    let keyboard = KeyboardMap::load(conn)?;
    for ch in text.chars() {
        let keysym = char_keysym(ch)?;
        let keycode = keyboard
            .keycode_for(keysym)
            .or_else(|| keyboard.keycode_for(ch.to_ascii_lowercase() as u32))
            .with_context(|| format!("character '{ch}' is not present in the X11 keymap"))?;
        let needs_shift = ch.is_ascii_uppercase()
            || matches!(
                ch,
                '!' | '@'
                    | '#'
                    | '$'
                    | '%'
                    | '^'
                    | '&'
                    | '*'
                    | '('
                    | ')'
                    | '_'
                    | '+'
                    | '{'
                    | '}'
                    | ':'
                    | '"'
                    | '<'
                    | '>'
                    | '?'
                    | '|'
            );
        if needs_shift {
            if let Some(shift) = keyboard.keycode_for(0xffe1) {
                send_xtest_input(conn, KEY_PRESS, shift, root, 0, 0)?;
                send_xtest_input(conn, KEY_PRESS, keycode, root, 0, 0)?;
                send_xtest_input(conn, KEY_RELEASE, keycode, root, 0, 0)?;
                send_xtest_input(conn, KEY_RELEASE, shift, root, 0, 0)?;
                continue;
            }
        }
        send_xtest_input(conn, KEY_PRESS, keycode, root, 0, 0)?;
        send_xtest_input(conn, KEY_RELEASE, keycode, root, 0, 0)?;
    }
    Ok(())
}

struct KeyboardMap {
    min_keycode: u8,
    keysyms_per_keycode: u8,
    keysyms: Vec<u32>,
}

impl KeyboardMap {
    fn load(conn: &RustConnection) -> anyhow::Result<Self> {
        let setup = conn.setup();
        let count = setup
            .max_keycode
            .saturating_sub(setup.min_keycode)
            .saturating_add(1);
        let reply = conn
            .get_keyboard_mapping(setup.min_keycode, count)
            .context("request X11 keyboard mapping")?
            .reply()
            .context("read X11 keyboard mapping")?;
        Ok(Self {
            min_keycode: setup.min_keycode,
            keysyms_per_keycode: reply.keysyms_per_keycode,
            keysyms: reply.keysyms,
        })
    }

    fn keycode_for(&self, keysym: u32) -> Option<u8> {
        self.keysyms
            .chunks(usize::from(self.keysyms_per_keycode))
            .position(|symbols| symbols.contains(&keysym))
            .and_then(|index| self.min_keycode.checked_add(index as u8))
    }
}

fn named_keysym(key: &str) -> Option<u32> {
    Some(match key.to_ascii_lowercase().as_str() {
        "return" | "enter" => 0xff0d,
        "tab" => 0xff09,
        "escape" | "esc" => 0xff1b,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        "shift" => 0xffe1,
        "control" | "ctrl" => 0xffe3,
        "alt" | "option" => 0xffe9,
        "super" | "cmd" | "meta" => 0xffeb,
        "space" => 0x20,
        _ => return None,
    })
}

fn char_keysym(ch: char) -> anyhow::Result<u32> {
    let shifted = match ch {
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '|' => '\\',
        other => other,
    };
    if shifted.is_ascii() {
        Ok(shifted as u32)
    } else {
        bail!("only ASCII typing is currently supported by cua-qgui-tool")
    }
}

fn parse_button(button: &str) -> anyhow::Result<u8> {
    Ok(match button {
        "left" => 1,
        "middle" => 2,
        "right" => 3,
        other => bail!("unsupported mouse button '{other}'"),
    })
}

fn motion(conn: &RustConnection, root: Window, x: i16, y: i16) -> anyhow::Result<()> {
    send_xtest_input(conn, MOTION_NOTIFY, 0, root, x, y)
}

fn send_xtest_input(
    conn: &RustConnection,
    event_type: u8,
    detail: u8,
    root: Window,
    x: i16,
    y: i16,
) -> anyhow::Result<()> {
    conn.xtest_fake_input(event_type, detail, 0, root, x, y, 0)
        .context("send XTEST input event")?
        .check()
        .context("confirm XTEST input event")
}

fn sleep_ms(ms: u64) {
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms.min(10_000)));
    }
}
