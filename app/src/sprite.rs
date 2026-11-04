// The web UI's icon sprite (src/renderer/index.html), drawn with resvg: same strokes,
// caps and joins as the browser rendered them. Generated — keep in sync with the sprite.

pub const SVG_HEAD: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">"##;

pub fn inner(name: &str) -> &'static str {
    match name {
        "home" => r##"<path d="M3 11l9-7 9 7v9a1 1 0 0 1-1 1h-5v-6H9v6H4a1 1 0 0 1-1-1z"/>"##,
        "monitor" => r##"<rect x="3" y="4" width="18" height="12" rx="1.5"/><path d="M8 20h8M12 16v4"/>"##,
        "arrow" => r##"<path d="M5 12h14M13 6l6 6-6 6"/>"##,
        "copy" => r##"<rect x="8" y="8" width="12" height="12" rx="1.5"/><path d="M16 8V5.5A1.5 1.5 0 0 0 14.5 4h-9A1.5 1.5 0 0 0 4 5.5v9A1.5 1.5 0 0 0 5.5 16H8"/>"##,
        "gear" => r##"<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/>"##,
        "chat" => r##"<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z"/>"##,
        "mic" => r##"<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>"##,
        "camera" => r##"<rect x="2.5" y="6" width="13.5" height="12" rx="2"/><path d="M16 10.5l5.5-3.5v10L16 13.5z"/>"##,
        "file" => r##"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M12 18v-6M9 15l3-3 3 3"/>"##,
        "expand" => r##"<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/>"##,
        "shrink" => r##"<path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5"/>"##,
        "power" => r##"<path d="M12 3v9M6.3 6.3a8 8 0 1 0 11.4 0"/>"##,
        "mouse" => r##"<rect x="6" y="3" width="12" height="18" rx="6"/><path d="M12 7v4"/>"##,
        "terminal" => r##"<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M7 9l3 3-3 3M12 15h5"/>"##,
        "clip" => r##"<rect x="6" y="4" width="12" height="17" rx="1.5"/><path d="M9 4V3h6v1M9 10h6M9 14h6"/>"##,
        "speaker" => r##"<path d="M4 9h4l5-4v14l-5-4H4z"/><path d="M16.5 8.5a5 5 0 0 1 0 7M19 6a8.5 8.5 0 0 1 0 12"/>"##,
        "share" => r##"<rect x="3" y="4" width="18" height="12" rx="1.5"/><path d="M12 13V7M9 10l3-3 3 3M8 20h8"/>"##,
        "key" => r##"<circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M17 6l3 3M15 8l2 2"/>"##,
        "shield" => r##"<path d="M12 3l8 3v6c0 4.5-3.4 8.2-8 9-4.6-.8-8-4.5-8-9V6z"/><path d="M9 12l2 2 4-4"/>"##,
        "x" => r##"<path d="M6 6l12 12M18 6L6 18"/>"##,
        "gauge" => r##"<path d="M4 18a8 8 0 1 1 16 0"/><path d="M12 18l4-6"/>"##,
        "keyboard" => r##"<rect x="2.5" y="6" width="19" height="12" rx="1.5"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M8 14h8"/>"##,
        // native only (the file manager)
        "folder" => r##"<path d="M3 7.5A1.5 1.5 0 0 1 4.5 6h4.2l2 2h8.8A1.5 1.5 0 0 1 21 9.5v8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z"/>"##,
        "doc" => r##"<path d="M14 3H7.5A1.5 1.5 0 0 0 6 4.5v15A1.5 1.5 0 0 0 7.5 21h9a1.5 1.5 0 0 0 1.5-1.5V7z"/><path d="M14 3v4h4"/>"##,
        "up" => r##"<path d="M12 19V5M6 11l6-6 6 6"/>"##,
        "refresh" => r##"<path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 4v7h-7"/>"##,
        "trash" => r##"<path d="M4 7h16M10 11v6M14 11v6M6 7l1 12.5A1.5 1.5 0 0 0 8.5 21h7a1.5 1.5 0 0 0 1.5-1.5L18 7M9 7V4.5A1.5 1.5 0 0 1 10.5 3h3A1.5 1.5 0 0 1 15 4.5V7"/>"##,
        "plus" => r##"<path d="M12 5v14M5 12h14"/>"##,
        "edit" => r##"<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>"##,
        "more" => r##"<circle cx="5" cy="12" r="1.3"/><circle cx="12" cy="12" r="1.3"/><circle cx="19" cy="12" r="1.3"/>"##,
        "back" => r##"<path d="M19 12H5M11 6l-6 6 6 6"/>"##,
        _ => "",
    }
}
