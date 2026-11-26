command -v openbox >/dev/null || exit 3
export DISPLAY=:1 XCURSOR_THEME=Adwaita
export PATH="$HOME/.local/bin:$PATH"
look="$HOME/.ferrobot/desktop"
# Zombies of a crashed run must not count as running.
alive() { pgrep -r R,S,D -x "$1" >/dev/null; }
if [ ! -f "$look/look-2" ]; then
  mkdir -p "$look" "$HOME/.config/openbox" "$HOME/.config/tint2" "$HOME/.config/lxterminal" \
    "$HOME/.config/gtk-3.0" "$HOME/.themes/Ferrobot/openbox-3"
  sed -e 's|<name>Clearlooks</name>|<name>Ferrobot</name>|' \
    -e 's|<name>sans</name>|<name>Inter</name>|g' -e 's|<size>8</size>|<size>10</size>|g' \
    -e 's|<number>4</number>|<number>1</number>|' -e 's|<titleLayout>NLIMC</titleLayout>|<titleLayout>LMC</titleLayout>|' \
    -e 's|</applications>|<application class="Chromium"><maximized>yes</maximized></application><application class="Lxterminal"><position force="no"><x>center</x><y>center</y></position></application><application class="Thunar"><position force="no"><x>center</x><y>center</y></position></application></applications>|' \
    /etc/xdg/openbox/rc.xml >"$HOME/.config/openbox/rc.xml"
  cat >"$HOME/.themes/Ferrobot/openbox-3/themerc" <<'EOF'
border.width: 1
padding.width: 10
padding.height: 7
window.handle.width: 0
window.client.padding.width: 0
window.client.padding.height: 0
window.active.border.color: #2c303b
window.inactive.border.color: #22252d
window.active.title.bg: flat solid
window.active.title.bg.color: #1c1e25
window.inactive.title.bg: flat solid
window.inactive.title.bg.color: #16181e
window.active.label.bg: parentrelative
window.inactive.label.bg: parentrelative
window.active.label.text.color: #e6e7ec
window.inactive.label.text.color: #7d808b
window.label.text.justify: center
window.active.button.unpressed.bg: parentrelative
window.active.button.unpressed.image.color: #a9acb8
window.active.button.hover.bg: flat solid
window.active.button.hover.bg.color: #2a2e39
window.active.button.hover.image.color: #ffffff
window.active.button.pressed.bg: flat solid
window.active.button.pressed.bg.color: #8197f2
window.active.button.pressed.image.color: #ffffff
window.active.button.disabled.bg: parentrelative
window.active.button.disabled.image.color: #4a4d57
window.active.button.toggled.bg: parentrelative
window.active.button.toggled.image.color: #8197f2
window.inactive.button.unpressed.bg: parentrelative
window.inactive.button.unpressed.image.color: #5c5f6a
window.inactive.button.hover.bg: flat solid
window.inactive.button.hover.bg.color: #22252d
window.inactive.button.hover.image.color: #c9cbd3
window.inactive.button.pressed.bg: flat solid
window.inactive.button.pressed.bg.color: #8197f2
window.inactive.button.pressed.image.color: #ffffff
window.inactive.button.disabled.bg: parentrelative
window.inactive.button.disabled.image.color: #3a3d46
window.inactive.button.toggled.bg: parentrelative
window.inactive.button.toggled.image.color: #6f7ccc
menu.border.width: 1
menu.border.color: #2c303b
menu.items.bg: flat solid
menu.items.bg.color: #1c1e25
menu.items.text.color: #e6e7ec
menu.items.disabled.text.color: #5c5f6a
menu.items.active.bg: flat solid
menu.items.active.bg.color: #8197f2
menu.items.active.text.color: #ffffff
menu.title.bg: flat solid
menu.title.bg.color: #16181e
menu.title.text.color: #a9acb8
menu.separator.color: #2c303b
osd.border.width: 1
osd.border.color: #2c303b
osd.bg: flat solid
osd.bg.color: #1c1e25
osd.label.bg: parentrelative
osd.label.text.color: #e6e7ec
osd.hilight.bg: flat solid
osd.hilight.bg.color: #8197f2
osd.unhilight.bg: flat solid
osd.unhilight.bg.color: #2c303b
EOF
  cat >"$HOME/.config/tint2/tint2rc" <<'EOF'
rounded = 14
border_width = 1
border_sides = TBLR
background_color = #15171d 90
border_color = #ffffff 12
rounded = 9
border_width = 0
background_color = #ffffff 9
background_color_hover = #ffffff 14
background_color_pressed = #8197f2 40
panel_items = L:C
panel_size = 0 52
panel_shrink = 1
panel_margin = 0 12
panel_padding = 10 7 8
panel_background_id = 1
panel_position = bottom center horizontal
panel_layer = top
panel_monitor = all
strut_policy = follow_size
wm_menu = 0
panel_dock = 0
autohide = 0
mouse_effects = 1
mouse_hover_icon_asb = 100 0 12
mouse_pressed_icon_asb = 100 0 0
separator = new
separator_background_id = 0
separator_color = #ffffff 14
separator_style = line
separator_size = 1
separator_padding = 4 6
launcher_padding = 0 0 6
launcher_background_id = 0
launcher_icon_background_id = 2
launcher_icon_size = 26
launcher_icon_theme = Adwaita
launcher_icon_theme_override = 0
startup_notifications = 0
launcher_tooltip = 1
launcher_item_app = /usr/share/applications/chromium.desktop
launcher_item_app = /usr/share/applications/lxterminal.desktop
launcher_item_app = /usr/share/applications/thunar.desktop
time1_format = %H:%M
time2_format = %a %d %b
time1_font = Inter Semi-Bold 10
time2_font = Inter 8
clock_font_color = #e6e7ec 100
clock_padding = 8 0
clock_background_id = 0
tooltip_show_timeout = 0.4
tooltip_hide_timeout = 0.1
tooltip_padding = 8 5
tooltip_background_id = 1
tooltip_font = Inter 9
tooltip_font_color = #e6e7ec 100
EOF
  cat >"$HOME/.config/lxterminal/lxterminal.conf" <<'EOF'
[general]
fontname=JetBrains Mono 11
scrollback=5000
bgcolor=#0f1117
fgcolor=#d8dae3
color_preset=Custom
palette_color_0=#1b1d24
palette_color_1=#ef6b73
palette_color_2=#7fd1a0
palette_color_3=#e8c27a
palette_color_4=#8197f2
palette_color_5=#c49cf2
palette_color_6=#6fc8d6
palette_color_7=#d8dae3
palette_color_8=#4a4e5a
palette_color_9=#ff8a91
palette_color_10=#9be3b6
palette_color_11=#f2d49a
palette_color_12=#9cb0ff
palette_color_13=#d8b6ff
palette_color_14=#8fdbe6
palette_color_15=#ffffff
cursorblinks=true
audiblebell=false
geometry_columns=100
geometry_rows=28
hidescrollbar=true
hidemenubar=true
disablef10=true
disableconfirm=true
EOF
  # Debian's chromium wrapper ignores flags from the environment; a container needs these.
  mkdir -p "$HOME/.local/bin"
  printf '#!/bin/sh\nexec /usr/bin/chromium --no-sandbox --disable-dev-shm-usage --disable-gpu --no-first-run --no-default-browser-check --password-store=basic --force-dark-mode --test-type "$@"\n' \
    >"$HOME/.local/bin/chromium"
  chmod +x "$HOME/.local/bin/chromium"
  printf 'vte-terminal { padding: 10px 12px; }\n' >"$HOME/.config/gtk-3.0/gtk.css"
  printf '[Settings]\ngtk-theme-name=Arc-Dark\ngtk-icon-theme-name=Adwaita\ngtk-font-name=Inter 10\ngtk-cursor-theme-name=Adwaita\ngtk-application-prefer-dark-theme=1\n' \
    >"$HOME/.config/gtk-3.0/settings.ini"
  # Our own wallpaper and icons, drawn here so the image carries no picture files.
  icons="$HOME/.local/share/icons/hicolor"
  mkdir -p "$icons/48x48/apps" "$icons/64x64/apps"
  rm -rf "$HOME/.cache/tint2"
  node - "$look" "$icons" <<'EOF'
const zlib = require("zlib"), fs = require("fs");
const [look, icons] = process.argv.slice(2);
const crcTable = Array.from({ length: 256 }, (_, n) => {
  for (let k = 0; k < 8; k++) n = n & 1 ? 0xedb88320 ^ (n >>> 1) : n >>> 1;
  return n >>> 0;
});
const crc = (b) => { let c = ~0; for (const x of b) c = crcTable[(c ^ x) & 255] ^ (c >>> 8); return ~c >>> 0; };
const chunk = (type, data) => {
  const body = Buffer.concat([Buffer.from(type), data]), out = Buffer.alloc(body.length + 8);
  out.writeUInt32BE(data.length, 0); body.copy(out, 4); out.writeUInt32BE(crc(body), body.length + 4);
  return out;
};
// An RGBA PNG; `paint(x, y)` returns [r, g, b, a], a in 0..1.
const png = (file, W, H, paint) => {
  const raw = Buffer.alloc((W * 4 + 1) * H);
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
    const c = paint(x + 0.5, y + 0.5), i = y * (W * 4 + 1) + 1 + x * 4;
    for (let k = 0; k < 3; k++) raw[i + k] = Math.min(255, Math.round(c[k]));
    raw[i + 3] = Math.round(c[3] * 255);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(W, 0); ihdr.writeUInt32BE(H, 4); ihdr.set([8, 6, 0, 0, 0], 8);
  fs.writeFileSync(file, Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(raw, { level: 9 })), chunk("IEND", Buffer.alloc(0))]));
};
const mix = (a, b, t) => a + (b - a) * t, glow = (d, w) => Math.exp(-(d * d) / (w * w));
const clamp = (n) => Math.max(0, Math.min(1, n));
png(`${look}/wallpaper.png`, 1280, 800, (x, y) => {
  const u = x / 1280, v = y / 800, t = v * 0.65 + u * 0.35;
  const d = v - (0.78 - 0.55 * u + 0.08 * Math.sin(5.5 * u + 0.7));
  const band = glow(d, 0.022) * 0.26 + glow(d, 0.12) * 0.2;
  const halo = glow(Math.hypot(u - 0.82, v - 0.18), 0.34) * 0.16;
  return [mix(14, 26, t) + band * 129 + halo * 160, mix(16, 22, t) + band * 151 + halo * 120,
    mix(24, 44, t) + band * 242 + halo * 230, 1];
});
// Signed distances in icon units; a shape covers a pixel by how far inside it is.
const box = (x, y, cx, cy, hw, hh, r) => {
  const qx = Math.abs(x - cx) - hw + r, qy = Math.abs(y - cy) - hh + r;
  return Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) + Math.min(Math.max(qx, qy), 0) - r;
};
const line = (x, y, ax, ay, bx, by, w) => {
  const dx = bx - ax, dy = by - ay, t = clamp(((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy));
  return Math.hypot(x - ax - t * dx, y - ay - t * dy) - w / 2;
};
const over = (base, rgb, cover) => {
  const a = cover + base[3] * (1 - cover);
  if (a === 0) return base;
  return [...rgb.map((v, i) => (v * cover + base[i] * base[3] * (1 - cover)) / a), a];
};
const icon = (name, draw) => [48, 64].forEach((size) => {
  const k = 64 / size, cover = (d) => clamp(0.5 - d / k);
  png(`${icons}/${size}x${size}/apps/${name}.png`, size, size, (x, y) => draw(x * k, y * k, cover));
});
const clear = [0, 0, 0, 0], shade = (y, top, bottom) => top.map((v, i) => mix(v, bottom[i], y / 64));
icon("lxterminal", (x, y, cover) => {
  let c = over(clear, shade(y, [48, 53, 70], [26, 29, 40]), cover(box(x, y, 32, 32, 27, 27, 13)));
  c = over(c, [143, 162, 255], cover(Math.min(line(x, y, 20, 23, 30, 32, 4.5), line(x, y, 30, 32, 20, 41, 4.5))));
  return over(c, [230, 231, 236], cover(line(x, y, 34, 41, 44, 41, 4.5)));
});
icon("org.xfce.thunar", (x, y, cover) => {
  const c = over(clear, [96, 116, 222], cover(box(x, y, 22, 20, 12, 7, 4)));
  return over(c, shade(y, [160, 176, 255], [112, 134, 238]), cover(box(x, y, 32, 36, 26, 18, 6)));
});
EOF
  printf '#define close_width 9\n#define close_height 9\nstatic unsigned char close_bits[] = {\n 0x01,0x01,0x82,0x00,0x44,0x00,0x28,0x00,0x10,0x00,0x28,0x00,0x44,0x00,0x82,0x00,0x01,0x01};\n' \
    >"$HOME/.themes/Ferrobot/openbox-3/close.xbm"
  printf '#define max_width 9\n#define max_height 9\nstatic unsigned char max_bits[] = {\n 0xff,0x01,0xff,0x01,0x01,0x01,0x01,0x01,0x01,0x01,0x01,0x01,0x01,0x01,0x01,0x01,0xff,0x01};\n' \
    >"$HOME/.themes/Ferrobot/openbox-3/max.xbm"
  printf '#define max_toggled_width 9\n#define max_toggled_height 9\nstatic unsigned char max_toggled_bits[] = {\n 0xfc,0x01,0x04,0x01,0x7f,0x01,0x41,0x01,0x41,0x01,0xc1,0x01,0x41,0x00,0x41,0x00,0x7f,0x00};\n' \
    >"$HOME/.themes/Ferrobot/openbox-3/max_toggled.xbm"
  touch "$look/look-2"
fi
if ! alive Xvfb; then
  rm -f /tmp/.X1-lock /tmp/.X11-unix/X1
  setsid Xvfb :1 -screen 0 1280x800x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
  for i in $(seq 50); do [ -e /tmp/.X11-unix/X1 ] && break; sleep 0.1; done
fi
if ! alive openbox; then
  setsid openbox >/tmp/openbox.log 2>&1 &
  xwallpaper --zoom "$look/wallpaper.png" >/dev/null 2>&1
fi
alive tint2 || { setsid tint2 >/tmp/tint2.log 2>&1 & }
exit 0
