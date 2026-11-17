// The kit's icon set (design/kit/icons.js) as a module: one outline set on a 20 px grid with a
// 1.6 px round stroke. The kit is the source of truth: change design/kit/icons.js first, then copy
// the bodies here. `GIcon.vue` renders them.

type IconDef = string | { body: string; slash?: boolean; vb?: number; sw?: number };

/** Stroke width in viewBox units for each rendered size, so the stroke stays near 1.5 px. */
const STROKE: Record<number, number> = { 12: 2.1, 14: 1.9, 16: 1.8, 20: 1.6, 24: 1.46, 32: 1.25, 40: 1.125 };

const SLASH = "M3.75 3.75 16.25 16.25";

const ICONS: Record<string, IconDef> = {
    /* ---------------------------------------------------------------- tools */
    arrow: '<path d="M4.75 15.25 14.5 5.5"/><path d="M8 5.25h6.75V12"/>',
    line: '<path d="M6.35 13.65 13.65 6.35"/><circle cx="5" cy="15" r="1.75"/><circle cx="15" cy="5" r="1.75"/>',
    rect: '<rect x="3.5" y="5" width="13" height="10" rx="2"/>',
    ellipse: '<ellipse cx="10" cy="10" rx="7" ry="5.5"/>',
    pen: '<path d="M12.6 4.4a1.9 1.9 0 0 1 2.7 0l.3.3a1.9 1.9 0 0 1 0 2.7L7.4 15.6l-3.4.8.8-3.4Z"/><path d="m11.3 5.7 3 3"/>',
    highlighter: '<path d="m8.6 13.2-2.8-2.8 6.6-6.6a1.4 1.4 0 0 1 2 0l.8.8a1.4 1.4 0 0 1 0 2Z"/><path d="m5.8 10.4-1.6 3.2 2.2 2.2 3.2-1.6"/><path d="M11.5 16.5h5"/>',
    text: '<path d="M5 5.25h10M10 5.25v10.5M8 15.75h4"/>',
    step: '<circle cx="10" cy="10" r="7"/><path d="M8.7 8.1 10.4 7v6.25"/>',
    blur: '<path d="M10 3.5S5.25 8.6 5.25 11.75a4.75 4.75 0 0 0 9.5 0C14.75 8.6 10 3.5 10 3.5Z"/><path d="M7.9 12.2a2.2 2.2 0 0 0 1.8 2" opacity=".65"/>',
    pixelate: '<rect x="3.5" y="3.5" width="13" height="13" rx="2"/><path class="f" d="M7.83 3.5h4.34v4.33H7.83zM3.5 7.83h4.33v4.34H3.5zM12.17 7.83h4.33v4.34h-4.33zM7.83 12.17h4.34v4.5H7.83z"/>',
    /* redrawn: a stage light casting one cone onto the floor, instead of the frame and lens that read as a camera */
    spotlight: '<path class="fo" d="M7.6 7.75h4.8l3.6 8H4Z"/><path d="M6.75 7.5h6.5a3.25 3.25 0 0 0-6.5 0Z"/><path d="M3.25 15.75h13.5"/>',
    crop: '<path d="M6 2.75V14h11.25"/><path d="M2.75 6H14v11.25"/>',
    undo: '<path d="M7.5 4.75 4.25 8l3.25 3.25"/><path d="M4.5 8h7.25a4 4 0 0 1 0 8H9"/>',
    redo: '<path d="m12.5 4.75 3.25 3.25-3.25 3.25"/><path d="M15.5 8H8.25a4 4 0 0 0 0 8H11"/>',
    textbg: '<rect class="fo" x="3" y="3.5" width="14" height="13" rx="2.5"/><path d="M6.75 13.5 10 6.25l3.25 7.25M8 11h4"/>',
    /* arrange: the shape that moves is the filled one, the other is the outline it passes */
    'bring-forward': '<path d="M7.25 7.25h-2a2 2 0 0 0-2 2v5.5a2 2 0 0 0 2 2h5.5a2 2 0 0 0 2-2v-2"/><rect class="fo" x="7.25" y="3.25" width="9.5" height="9.5" rx="2"/><rect x="7.25" y="3.25" width="9.5" height="9.5" rx="2"/>',
    'send-backward': '<path class="fo" d="M7.25 7.25h-2a2 2 0 0 0-2 2v5.5a2 2 0 0 0 2 2h5.5a2 2 0 0 0 2-2v-2H7.25Z"/><path d="M7.25 7.25h-2a2 2 0 0 0-2 2v5.5a2 2 0 0 0 2 2h5.5a2 2 0 0 0 2-2v-2"/><rect x="7.25" y="3.25" width="9.5" height="9.5" rx="2"/>',

    /* -------------------------------------------------------------- actions */
    copy: '<rect x="7" y="7" width="9.5" height="9.5" rx="2.25"/><path d="M13 7V5.75a2.25 2.25 0 0 0-2.25-2.25h-5A2.25 2.25 0 0 0 3.5 5.75v5A2.25 2.25 0 0 0 5.75 13H7"/>',
    save: '<path d="M10 3.5V12"/><path d="M6.5 8.75 10 12.25l3.5-3.5"/><path d="M3.75 12.5v2a2 2 0 0 0 2 2h8.5a2 2 0 0 0 2-2v-2"/>',
    /* save with a pencil: the same tray and arrow, moved left, and a name to write */
    'save-as': '<path d="M7.5 3.5v8"/><path d="m4.25 8.25 3.25 3.25 3.25-3.25"/><path d="M3.75 12.5v2a2 2 0 0 0 2 2h8.5a2 2 0 0 0 2-2v-2"/><path d="m12.5 11 .63-1.63 2.62-2.62 1 1-2.62 2.62Z"/>',
    upload: '<path d="M7 15.5H5.75a3.25 3.25 0 0 1-.15-6.45 4.5 4.5 0 0 1 8.8 0 3.25 3.25 0 0 1-.15 6.45H13"/><path d="M10 16.75V10.5"/><path d="M7.75 12.75 10 10.5l2.25 2.25"/>',
    ocr: '<path d="M3.5 7V5.5a2 2 0 0 1 2-2H7M13 3.5h1.5a2 2 0 0 1 2 2V7M16.5 13v1.5a2 2 0 0 1-2 2H13M7 16.5H5.5a2 2 0 0 1-2-2V13"/><path d="M7 7.75h6M7 10h6M7 12.25h3.5"/>',
    pin: '<path d="M7.25 3.5h5.5"/><path d="M8.25 3.5v4.75l-2.5 3h8.5l-2.5-3V3.5"/><path d="M10 11.25v5.25"/>',
    record: '<circle cx="10" cy="10" r="6.75"/><circle class="rec" cx="10" cy="10" r="3.6"/>',
    close: '<path d="m5.5 5.5 9 9M14.5 5.5l-9 9"/>',
    /* a capture with the plus badge of a copy drop, for dragging a capture out as a file */
    drop: '<path d="M9.5 13.75h-4a2.25 2.25 0 0 1-2.25-2.25v-6A2.25 2.25 0 0 1 5.5 3.25h6a2.25 2.25 0 0 1 2.25 2.25v4"/><circle cx="13.75" cy="13.75" r="3"/><path d="M13.75 12.25v3M12.25 13.75h3"/>',

    /* ------------------------------------------------- capture and recording */
    display: '<rect x="2.75" y="3.5" width="14.5" height="10" rx="2"/><path d="M10 13.5v3M7 16.5h6"/>',
    window: '<rect x="2.75" y="4.25" width="14.5" height="11.5" rx="2.25"/><path d="M2.75 7.75h14.5"/>',
    area: '<path d="M3.25 6.5v-.75a1.5 1.5 0 0 1 1.5-1.5H6M14 4.25h1.25a1.5 1.5 0 0 1 1.5 1.5v.75M16.75 13.5v.75a1.5 1.5 0 0 1-1.5 1.5H14M6 15.75H4.75a1.5 1.5 0 0 1-1.5-1.5v-.75M9 4.25h2M9 15.75h2M3.25 9v2M16.75 9v2"/>',
    cursor: '<path d="M5.5 3.5v11.6l3.05-2.95 2.05 4.6 2.15-.95-2.05-4.55h4.25Z"/>',
    /* repeat last area: a closed loop, unlike retry, which is one open turn */
    repeat: '<path d="M4.25 9.25V8.5a2.5 2.5 0 0 1 2.5-2.5h9"/><path d="m13.5 3.75 2.25 2.25-2.25 2.25"/><path d="M15.75 10.75v.75a2.5 2.5 0 0 1-2.5 2.5h-9"/><path d="m6.5 16.25-2.25-2.25 2.25-2.25"/>',
    click: '<path d="M9.25 8.75v8.1l2.1-2.05 1.45 3.2 1.55-.7-1.45-3.15h2.95Z"/><path d="M9.25 3v1.6M4.75 4.85l1.1 1.1M3 9.35h1.6M13.75 4.85l-1.1 1.1"/>',
    microphone: '<rect x="7.25" y="2.75" width="5.5" height="9" rx="2.75"/><path d="M4.75 9.25a5.25 5.25 0 0 0 10.5 0M10 14.5v2.75M7.5 17.25h5"/>',
    'microphone-off': { slash: true, body: '<rect x="7.25" y="2.75" width="5.5" height="9" rx="2.75"/><path d="M4.75 9.25a5.25 5.25 0 0 0 10.5 0M10 14.5v2.75M7.5 17.25h5"/>' },
    speaker: '<path d="M3.5 8.1v3.8c0 .47.38.85.85.85h2.3l3.6 3.1a.5.5 0 0 0 .83-.38V4.53a.5.5 0 0 0-.83-.38l-3.6 3.1h-2.3a.85.85 0 0 0-.85.85Z"/><path d="M12.9 7.9a3 3 0 0 1 0 4.2M15 5.75a6 6 0 0 1 0 8.5"/>',
    'speaker-off': { slash: true, body: '<path d="M3.5 8.1v3.8c0 .47.38.85.85.85h2.3l3.6 3.1a.5.5 0 0 0 .83-.38V4.53a.5.5 0 0 0-.83-.38l-3.6 3.1h-2.3a.85.85 0 0 0-.85.85Z"/><path d="M12.9 7.9a3 3 0 0 1 0 4.2M15 5.75a6 6 0 0 1 0 8.5"/>' },
    webcam: '<circle cx="10" cy="8.5" r="5.75"/><circle cx="10" cy="8.5" r="2"/><path d="M10 14.25v2.75M6.75 17h6.5"/>',
    'webcam-off': { slash: true, body: '<circle cx="10" cy="8.5" r="5.75"/><circle cx="10" cy="8.5" r="2"/><path d="M10 14.25v2.75M6.75 17h6.5"/>' },
    timer: '<circle cx="10" cy="11" r="6"/><path d="M8.25 2.75h3.5M10 2.75V5M10 11V8.25M15.1 5.9l.9-.9"/>',
    pause: '<path d="M7.25 4.75v10.5M12.75 4.75v10.5"/>',
    play: '<path d="M6.25 4.5v11l9-5.5Z"/>',
    stop: '<rect x="5" y="5" width="10" height="10" rx="2.25"/>',
    trim: '<path d="M2.75 10h2.25M15 10h2.25"/><path class="fo" d="M6.5 6.25h7v7.5h-7z"/><path d="M8 4.75H6.5a1 1 0 0 0-1 1v8.5a1 1 0 0 0 1 1H8M12 4.75h1.5a1 1 0 0 1 1 1v8.5a1 1 0 0 1-1 1H12"/>',
    scissors: '<circle cx="6" cy="14.25" r="2.5"/><circle cx="14" cy="14.25" r="2.5"/><path d="M7.6 12.35 14.25 3.5M12.4 12.35 5.75 3.5"/>',

    /* ----------------------------------------------------------------- media */
    image: '<rect x="3" y="3.75" width="14" height="12.5" rx="2.25"/><circle cx="7.4" cy="7.9" r="1.35"/><path d="m3.25 14.5 4.1-3.6a1 1 0 0 1 1.3-.02l2.1 1.72 2.37-1.95a1 1 0 0 1 1.3.03l2.33 2.07"/>',
    film: '<rect x="3.5" y="3.25" width="13" height="13.5" rx="2.25"/><path d="M6.75 3.25v13.5M13.25 3.25v13.5M3.5 7.75h3.25M3.5 12.25h3.25M13.25 7.75h3.25M13.25 12.25h3.25"/>',
    gif: '<rect x="2.75" y="4.75" width="14.5" height="10.5" rx="2.25"/><path d="M8.6 8.3a1.9 1.9 0 0 0-1.65-1h-.2a1.75 1.75 0 0 0-1.75 1.75v1.9a1.75 1.75 0 0 0 1.75 1.75h.2a1.65 1.65 0 0 0 1.65-1.65v-.55H7.2M10.85 7.3v5.4M13.25 12.7V7.3h2.25M13.25 10h1.85"/>',
    folder: '<path d="M3.5 6.25A1.75 1.75 0 0 1 5.25 4.5H8l1.5 1.75h5.25a1.75 1.75 0 0 1 1.75 1.75v6.25A1.75 1.75 0 0 1 14.75 16H5.25a1.75 1.75 0 0 1-1.75-1.75Z"/>',

    /* ------------------------------------------------------- app and windows */
    search: '<circle cx="8.75" cy="8.75" r="5"/><path d="m12.4 12.4 3.85 3.85"/>',
    filter: '<path d="M3.75 5.75h12.5M6.25 10h7.5M8.75 14.25h2.5"/>',
    grid: '<rect x="3.5" y="3.5" width="5.25" height="5.25" rx="1.4"/><rect x="11.25" y="3.5" width="5.25" height="5.25" rx="1.4"/><rect x="3.5" y="11.25" width="5.25" height="5.25" rx="1.4"/><rect x="11.25" y="11.25" width="5.25" height="5.25" rx="1.4"/>',
    list: '<path d="M7.75 5.75h8.5M7.75 10h8.5M7.75 14.25h8.5M3.9 5.75h.01M3.9 10h.01M3.9 14.25h.01"/>',
    sidebar: '<rect x="2.75" y="3.75" width="14.5" height="12.5" rx="2.25"/><path d="M8 3.75v12.5M5 7h.75M5 9.5h.75"/>',
    settings: '<path d="M8.57 4.84 8.82 2.95h2.36l.25 1.89a5.35 5.35 0 0 1 1.2.5l1.52-1.16 1.67 1.67-1.16 1.52a5.35 5.35 0 0 1 .5 1.2l1.89.25v2.36l-1.89.25a5.35 5.35 0 0 1-.5 1.2l1.16 1.52-1.67 1.67-1.52-1.16a5.35 5.35 0 0 1-1.2.5l-.25 1.89H8.82l-.25-1.89a5.35 5.35 0 0 1-1.2-.5l-1.52 1.16-1.67-1.67 1.16-1.52a5.35 5.35 0 0 1-.5-1.2l-1.89-.25V8.82l1.89-.25a5.35 5.35 0 0 1 .5-1.2L4.18 5.85l1.67-1.67 1.52 1.16a5.35 5.35 0 0 1 1.2-.5Z"/><circle cx="10" cy="10" r="2.35"/>',
    keyboard: '<rect x="2.5" y="5" width="15" height="10" rx="2.25"/><path d="M5.75 8.25h.01M8.5 8.25h.01M11.5 8.25h.01M14.25 8.25h.01M7 11.75h6"/>',
    cloud: '<path d="M5.75 14.75a3.25 3.25 0 0 1-.15-6.45 4.5 4.5 0 0 1 8.8 0 3.25 3.25 0 0 1-.15 6.45Z"/>',
    link: '<path d="m8.25 11.75 3.5-3.5"/><path d="m9.4 6.1 1.1-1.1a3.18 3.18 0 0 1 4.5 4.5l-1.1 1.1"/><path d="m10.6 13.9-1.1 1.1a3.18 3.18 0 0 1-4.5-4.5l1.1-1.1"/>',
    'external-link': '<path d="M11.25 3.75h5v5"/><path d="m16 4-6.5 6.5"/><path d="M14.5 11.5v3a2 2 0 0 1-2 2h-7a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2h3"/>',
    key: '<circle cx="7" cy="13" r="3.25"/><path d="m9.35 10.65 6.9-6.9M13.75 6.25l2 2M11.75 8.25l1.5 1.5"/>',
    lock: '<rect x="4.5" y="8.75" width="11" height="8" rx="2"/><path d="M7 8.75V6.5a3 3 0 0 1 6 0v2.25"/>',
    unlock: '<rect x="4.5" y="8.75" width="11" height="8" rx="2"/><path d="M7 8.75V6.5a3 3 0 0 1 5.82-1.03"/>',
    shield: '<path d="M10 2.75 4.5 4.9v4.6c0 3.45 2.3 6.1 5.5 7.6 3.2-1.5 5.5-4.15 5.5-7.6V4.9Z"/>',
    globe: '<circle cx="10" cy="10" r="7"/><path d="M3.25 10h13.5M10 3c-1.9 2-2.85 4.35-2.85 7s.95 5 2.85 7c1.9-2 2.85-4.35 2.85-7S11.9 5 10 3Z"/>',
    history: '<path d="M4.25 10a5.75 5.75 0 1 0 1.7-4.1"/><path d="M5.5 2.75v3.5H9"/><path d="M10 7v3.25l2.25 1.5"/>',
    /* a text field cut by the insertion bar */
    rename: '<path d="M9 6.25H5a1.75 1.75 0 0 0-1.75 1.75v4c0 .97.78 1.75 1.75 1.75h4M14 6.25h1a1.75 1.75 0 0 1 1.75 1.75v4c0 .97-.78 1.75-1.75 1.75h-1"/><path d="M11.5 3.75v12.5M9.75 3.75h3.5M9.75 16.25h3.5"/>',
    /* the macOS accessibility figure: a person with open arms in a ring */
    accessibility: '<circle cx="10" cy="10" r="7"/><circle class="f" cx="10" cy="6.25" r="1.1"/><path d="M6.75 8.5 10 9.25l3.25-.75M10 9.25v2.5l-1.75 2.75M10 11.75l1.75 2.75"/>',
    /* quit: the power sign */
    quit: '<path d="M10 2.75v6.5"/><path d="M6.25 5.5a6.25 6.25 0 1 0 7.5 0"/>',
    trash: '<path d="M3.75 5.75h12.5M8 5.5V4.4c0-.5.4-.9.9-.9h2.2c.5 0 .9.4.9.9v1.1M5.25 5.75l.68 9.5a1.6 1.6 0 0 0 1.6 1.5h4.94a1.6 1.6 0 0 0 1.6-1.5l.68-9.5M8.4 9v4.75M11.6 9v4.75"/>',
    star: '<path d="M10 3.1 11.85 8.15 17.23 8.35 13 11.67 14.47 16.85 10 13.85 5.53 16.85 7 11.67 2.77 8.35 8.15 8.15Z"/>',
    eye: '<path d="M2.75 10c1.4-3.1 4-5 7.25-5s5.85 1.9 7.25 5c-1.4 3.1-4 5-7.25 5s-5.85-1.9-7.25-5Z"/><circle cx="10" cy="10" r="2.25"/>',
    'eye-off': { slash: true, body: '<path d="M2.75 10c1.4-3.1 4-5 7.25-5s5.85 1.9 7.25 5c-1.4 3.1-4 5-7.25 5s-5.85-1.9-7.25-5Z"/><circle cx="10" cy="10" r="2.25"/>' },
    opacity: '<circle cx="10" cy="10" r="6.75"/><path class="fo" d="M10 3.25a6.75 6.75 0 0 1 0 13.5Z"/><path d="M10 3.25v13.5"/>',
    'zoom-in': '<circle cx="8.75" cy="8.75" r="5"/><path d="m12.4 12.4 3.85 3.85M8.75 6.75v4M6.75 8.75h4"/>',
    'zoom-out': '<circle cx="8.75" cy="8.75" r="5"/><path d="m12.4 12.4 3.85 3.85M6.75 8.75h4"/>',
    fit: '<path d="M12 3.5h4.5V8M16.25 3.75 11.5 8.5M8 16.5H3.5V12M3.75 16.25 8.5 11.5"/>',
    grip: '<circle class="f" cx="7.5" cy="5.5" r="1.15"/><circle class="f" cx="12.5" cy="5.5" r="1.15"/><circle class="f" cx="7.5" cy="10" r="1.15"/><circle class="f" cx="12.5" cy="10" r="1.15"/><circle class="f" cx="7.5" cy="14.5" r="1.15"/><circle class="f" cx="12.5" cy="14.5" r="1.15"/>',
    more: '<circle class="f" cx="4.75" cy="10" r="1.3"/><circle class="f" cx="10" cy="10" r="1.3"/><circle class="f" cx="15.25" cy="10" r="1.3"/>',
    plus: '<path d="M10 4.5v11M4.5 10h11"/>',
    minus: '<path d="M4.5 10h11"/>',

    /* ---------------------------------------------------------------- status */
    check: '<path d="m5 10.5 3.25 3.25L15 6.5"/>',
    info: '<circle cx="10" cy="10" r="7"/><path d="M10 9.25v4.5"/><circle class="f" cx="10" cy="6.6" r=".95"/>',
    warning: '<path d="M8.7 4.25a1.5 1.5 0 0 1 2.6 0l5.55 9.75a1.5 1.5 0 0 1-1.3 2.25H4.45a1.5 1.5 0 0 1-1.3-2.25Z"/><path d="M10 8.25v3"/><circle class="f" cx="10" cy="13.6" r=".85"/>',
    bang: '<path d="M10 5.5v5.5"/><circle class="f" cx="10" cy="14.5" r="1.1"/>',
    retry: '<path d="M15.75 10a5.75 5.75 0 1 1-1.7-4.1"/><path d="M14.5 2.75v3.5H11"/>',
    qr: '<rect x="3.5" y="3.5" width="5" height="5" rx="1"/><rect x="11.5" y="3.5" width="5" height="5" rx="1"/><rect x="3.5" y="11.5" width="5" height="5" rx="1"/><path d="M11.5 11.5h2v2M16.5 11.5v.01M11.5 16.5v.01M14 16.5h2.5V14"/>',

    /* ------------------------------------------------------------ navigation */
    'chevron-up': '<path d="m6.5 11.75 3.5-3.5 3.5 3.5"/>',
    'chevron-down': '<path d="m6.5 8.25 3.5 3.5 3.5-3.5"/>',
    'chevron-left': '<path d="M11.75 6.5 8.25 10l3.5 3.5"/>',
    'chevron-right': '<path d="m8.25 6.5 3.5 3.5-3.5 3.5"/>',
    'chevron-up-down': '<path d="m7 7.5 3-3 3 3M7 12.5l3 3 3-3"/>',

    /* ----------------------------------------------- menu bar glyph, 16 px grid */
    glyph: { vb: 16, sw: 1.5, body: '<path d="M2.25 5.5V3.9c0-.91.74-1.65 1.65-1.65h1.6M10.5 2.25h1.6c.91 0 1.65.74 1.65 1.65v1.6M13.75 10.5v1.6c0 .91-.74 1.65-1.65 1.65h-1.6M5.5 13.75H3.9c-.91 0-1.65-.74-1.65-1.65v-1.6"/><circle cx="8" cy="8" r="2.35"/>' },
    'glyph-recording': { vb: 16, sw: 1.5, body: '<path d="M2.25 5.5V3.9c0-.91.74-1.65 1.65-1.65h1.6M10.5 2.25h1.6c.91 0 1.65.74 1.65 1.65v1.6M13.75 10.5v1.6c0 .91-.74 1.65-1.65 1.65h-1.6M5.5 13.75H3.9c-.91 0-1.65-.74-1.65-1.65v-1.6"/><circle class="rec" cx="8" cy="8" r="3.15"/>' },
  };

/** Older names from the round 1 prototype. */
const ALIASES: Record<string, string> = {
    chev: 'chevron-down', open: 'external-link', alert: 'warning', 'text-recognition': 'ocr',
    rectangle: 'rect', clock: 'history', gear: 'settings', 'microphone-on': 'microphone', mic: 'microphone',
    'mic-off': 'microphone-off', refresh: 'retry', 'drag': 'grip', ellipsis: 'more', camera: 'webcam',
    'camera-off': 'webcam-off', power: 'quit', loop: 'repeat', forward: 'bring-forward', backward: 'send-backward',
  };

let uid = 0;

function resolve(name: string): string | null {
  if (ICONS[name]) return name;
  const alias = ALIASES[name];
  return alias && ICONS[alias] ? alias : null;
}

/** Class shorthands inside the bodies become presentation attributes. */
function paint(body: string): string {
  return body
    .replace(/class="f"/g, 'fill="currentColor" stroke="none"')
    .replace(/class="fo"/g, 'fill="currentColor" fill-opacity=".38" stroke="none"')
    .replace(/class="rec"/g, 'stroke="none" style="fill:var(--rec,#FF5A50)"');
}

/** The SVG markup of an icon; `label` makes it an announced image, otherwise it is hidden. */
export function iconSvg(name: string, size = 20, label?: string): string {
  const key = resolve(name);
  if (!key) return `<svg viewBox="0 0 20 20" width="${size}" height="${size}" aria-hidden="true"></svg>`;
  const raw = ICONS[key];
  const def = typeof raw === "string" ? { body: raw } : raw;
  const vb = def.vb ?? 20;
  const sw = def.sw ?? STROKE[size] ?? 1.6;
  let body = paint(def.body);
  if (def.slash) {
    const id = `g-icon-cut-${++uid}`;
    body =
      `<mask id="${id}" maskUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">` +
      `<rect width="20" height="20" fill="#fff"/><path d="${SLASH}" stroke="#000" stroke-width="${(sw + 2.8).toFixed(2)}" stroke-linecap="round"/></mask>` +
      `<g mask="url(#${id})">${body}</g><path d="${SLASH}"/>`;
  }
  const a11y = label ? ` role="img" aria-label="${label.replace(/"/g, "&quot;")}"` : ' aria-hidden="true" focusable="false"';
  return `<svg class="g-icon-svg" viewBox="0 0 ${vb} ${vb}" width="${size}" height="${size}" fill="none" stroke="currentColor" stroke-width="${sw}" stroke-linecap="round" stroke-linejoin="round"${a11y}>${body}</svg>`;
}

export function hasIcon(name: string): boolean {
  return resolve(name) !== null;
}
