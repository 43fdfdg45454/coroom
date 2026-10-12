// Administración de coroom: sin frameworks ni dependencias. Habla con la API `/api/v1` con el
// token de la sesión web (en sessionStorage: se borra al cerrar la pestaña).
"use strict";

const API = "/api/v1";
const TOKEN_KEY = "coroom.admin.token";
const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => Array.from(root.querySelectorAll(selector));

let token = sessionStorage.getItem(TOKEN_KEY);
let me = null;
let roomsTimer = null;

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function api(method, path, body) {
  const headers = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (token) headers.Authorization = `Bearer ${token}`;
  let response;
  try {
    response = await fetch(API + path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      cache: "no-store",
    });
  } catch (err) {
    throw new ApiError(0, "network", "No se pudo hablar con el servidor. ¿Sigue en marcha?");
  }
  const text = await response.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { data = null; }
  if (!response.ok) {
    const message = data && data.message ? data.message : `El servidor respondió ${response.status}.`;
    if (response.status === 401 && token) {
      setToken(null);
      show("login");
    }
    throw new ApiError(response.status, data && data.error, message);
  }
  return data;
}

function setToken(value) {
  token = value;
  if (value) sessionStorage.setItem(TOKEN_KEY, value);
  else sessionStorage.removeItem(TOKEN_KEY);
}

function notify(message, bad = false) {
  const status = $("#status");
  status.textContent = message;
  status.classList.toggle("bad", bad);
  status.hidden = false;
  clearTimeout(notify.timer);
  notify.timer = setTimeout(() => { status.hidden = true; }, bad ? 6000 : 3500);
}

function formError(form, message) {
  const error = $(".error", form);
  error.textContent = message || "";
  error.hidden = !message;
}

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (value !== false && value !== null && value !== undefined) node.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children) {
    if (child === null || child === undefined) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

const dateFormat = new Intl.DateTimeFormat("es", { dateStyle: "medium", timeStyle: "short" });
function when(seconds) {
  return seconds ? dateFormat.format(new Date(seconds * 1000)) : "—";
}

function bytes(value) {
  if (value === null || value === undefined) return "—";
  const units = ["B", "kB", "MB", "GB", "TB"];
  let n = value;
  let unit = 0;
  while (n >= 1000 && unit < units.length - 1) { n /= 1000; unit += 1; }
  return `${n.toLocaleString("es", { maximumFractionDigits: unit === 0 ? 0 : 1 })} ${units[unit]}`;
}

function show(view) {
  for (const name of ["setup", "login", "app"]) $(`#view-${name}`).hidden = name !== view;
  $("#session").hidden = view !== "app";
  if (view !== "app" && roomsTimer) { clearInterval(roomsTimer); roomsTimer = null; }
  const first = $(`#view-${view} input`);
  if (first) first.focus();
}

function busy(form, on) {
  for (const button of $$("button", form)) button.disabled = on;
}

// --- Arranque, primer administrador e inicio de sesión ------------------------------------------

async function boot() {
  try {
    const setup = await api("GET", "/setup");
    $("#server-host").textContent = setup.server_host;
    $("#server-version").textContent = `v${setup.version}`;
    document.title = `coroom · ${setup.server_host}`;
    if (setup.needed) return show("setup");
  } catch (err) {
    notify(err.message, true);
  }
  if (!token) return show("login");
  try {
    me = await api("GET", "/me");
    enterApp();
  } catch {
    show("login");
  }
}

$("#setup-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const data = Object.fromEntries(new FormData(form));
  if (!form.checkValidity()) return formError(form, "Revisa los campos: el usuario de 3 a 32 caracteres y la contraseña de al menos 12.");
  if (data.password !== data.password2) return formError(form, "Las contraseñas no coinciden.");
  formError(form, "");
  busy(form, true);
  try {
    await api("POST", "/setup", { username: data.username, password: data.password, display_name: data.display_name || undefined });
    const session = await api("POST", "/auth/login", { username: data.username, password: data.password });
    setToken(session.token);
    me = { user: session.user };
    notify("Administrador creado. Ya puedes dar de alta a tus amigos.");
    enterApp();
  } catch (err) {
    formError(form, err.message);
  } finally {
    busy(form, false);
  }
});

$("#login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const data = Object.fromEntries(new FormData(form));
  if (!data.username || !data.password) return formError(form, "Escribe tu usuario y tu contraseña.");
  formError(form, "");
  busy(form, true);
  try {
    const session = await api("POST", "/auth/login", { username: data.username, password: data.password });
    setToken(session.token);
    me = { user: session.user };
    form.reset();
    enterApp();
  } catch (err) {
    formError(form, err.message);
  } finally {
    busy(form, false);
  }
});

$("#logout").addEventListener("click", async () => {
  try { await api("POST", "/auth/logout"); } catch { /* ya cerrada */ }
  setToken(null);
  me = null;
  show("login");
});

function enterApp() {
  $("#session-user").textContent = me.user.display_name || me.user.username;
  show("app");
  select(location.hash.slice(1) || "users");
}

// --- Navegación -------------------------------------------------------------------------------

const loaders = { users: loadUsers, invitations: loadInvitations, usage: loadUsage, rooms: loadRooms, settings: loadSettings };

function select(section) {
  if (!loaders[section]) section = "users";
  for (const button of $$(".sidebar button")) {
    if (button.dataset.section === section) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  }
  for (const node of $$(".content > section")) node.hidden = node.id !== `section-${section}`;
  history.replaceState(null, "", `#${section}`);
  if (roomsTimer) { clearInterval(roomsTimer); roomsTimer = null; }
  if (section === "rooms") roomsTimer = setInterval(() => loadRooms().catch(() => {}), 5000);
  loaders[section]().catch((err) => notify(err.message, true));
}

for (const button of $$(".sidebar button")) {
  button.addEventListener("click", () => {
    select(button.dataset.section);
    $("#principal").focus();
  });
}

// Flechas arriba y abajo en la barra lateral, como en las listas de GNOME y KDE.
$(".sidebar").addEventListener("keydown", (event) => {
  const buttons = $$(".sidebar button");
  const index = buttons.indexOf(document.activeElement);
  if (index < 0) return;
  const step = { ArrowDown: 1, ArrowRight: 1, ArrowUp: -1, ArrowLeft: -1 }[event.key];
  if (!step) return;
  event.preventDefault();
  buttons[(index + step + buttons.length) % buttons.length].focus();
});

// --- Usuarios ---------------------------------------------------------------------------------

async function loadUsers() {
  const { users } = await api("GET", "/admin/users");
  const body = $("#users-body");
  body.replaceChildren();
  $("#users-empty").hidden = users.length > 0;
  for (const user of users) {
    const self = me && me.user.id === user.id;
    const state = user.disabled
      ? el("span", { class: "pill danger", text: "Deshabilitado" })
      : el("span", { class: "pill ok" }, el("span", { class: `dot${user.online ? " on" : ""}`, "aria-hidden": "true" }), user.online ? "Conectado" : "Activo");
    const actions = el("td", { class: "row-actions" },
      el("button", {
        type: "button", class: "small", text: user.is_admin ? "Quitar admin" : "Hacer admin",
        "aria-label": `${user.is_admin ? "Quitar administrador a" : "Hacer administrador a"} ${user.username}`,
        onclick: () => patchUser(user, { is_admin: !user.is_admin }),
      }),
      el("button", {
        type: "button", class: "small", text: "Contraseña",
        "aria-label": `Restablecer la contraseña de ${user.username}`,
        onclick: () => openPassword(user),
      }),
      self ? null : el("button", {
        type: "button", class: `small${user.disabled ? "" : " danger"}`, text: user.disabled ? "Habilitar" : "Deshabilitar",
        "aria-label": `${user.disabled ? "Habilitar" : "Deshabilitar"} a ${user.username}`,
        onclick: () => {
          if (!user.disabled && !confirm(`¿Deshabilitar a ${user.username}? Se cerrarán sus sesiones.`)) return;
          patchUser(user, { disabled: !user.disabled });
        },
      }));
    body.append(el("tr", {},
      el("td", { class: "who" }, el("strong", { text: user.display_name }), el("span", { text: `@${user.username}${self ? " · tú" : ""}` })),
      el("td", {}, user.is_admin ? el("span", { class: "pill accent", text: "Administrador" }) : "Usuario"),
      el("td", {}, state),
      el("td", { class: "num", text: user.devices }),
      el("td", { text: when(user.last_seen_at) }),
      actions));
  }
}

async function patchUser(user, change) {
  try {
    await api("PATCH", `/admin/users/${user.id}`, change);
    notify(`${user.username}: cambios guardados.`);
    await loadUsers();
  } catch (err) {
    notify(err.message, true);
  }
}

$("#user-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const data = Object.fromEntries(new FormData(form));
  if (!form.checkValidity()) return formError(form, "El usuario debe tener de 3 a 32 caracteres (letras, dígitos, «.», «-», «_») y la contraseña al menos 12.");
  formError(form, "");
  busy(form, true);
  try {
    const user = await api("POST", "/admin/users", {
      username: data.username,
      password: data.password,
      display_name: data.display_name || undefined,
      is_admin: data.is_admin === "on",
    });
    form.reset();
    notify(`Usuario ${user.username} creado. Puede entrar con «coroom login».`);
    await loadUsers();
  } catch (err) {
    formError(form, err.message);
  } finally {
    busy(form, false);
  }
});

let passwordUser = null;
function openPassword(user) {
  passwordUser = user;
  const dialog = $("#password-dialog");
  $("#password-user").textContent = user.username;
  formError($("#password-form"), "");
  $("#password-form").reset();
  $("#password-form [name=username]").value = user.username;
  dialog.showModal();
}

$("#password-cancel").addEventListener("click", () => $("#password-dialog").close());
$("#password-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const password = new FormData(form).get("password");
  if (!form.checkValidity()) return formError(form, "La contraseña debe tener al menos 12 caracteres.");
  busy(form, true);
  try {
    await api("POST", `/admin/users/${passwordUser.id}/password`, { password });
    $("#password-dialog").close();
    notify(`Contraseña de ${passwordUser.username} cambiada; sus sesiones se han cerrado.`);
  } catch (err) {
    formError(form, err.message);
  } finally {
    busy(form, false);
  }
});

// --- Invitaciones -----------------------------------------------------------------------------

const invitationStates = {
  pending: ["Pendiente", "accent"],
  used: ["Canjeada", "ok"],
  expired: ["Caducada", "warn"],
  revoked: ["Revocada", "danger"],
};

async function loadInvitations() {
  const { invitations } = await api("GET", "/admin/invitations");
  const body = $("#invitations-body");
  body.replaceChildren();
  $("#invitations-empty").hidden = invitations.length > 0;
  for (const invitation of invitations) {
    const [label, tone] = invitationStates[invitation.state] || [invitation.state, ""];
    body.append(el("tr", {},
      el("td", {}, el("code", { text: `····-····-${invitation.hint}` })),
      el("td", { text: invitation.note || "—" }),
      el("td", {}, el("span", { class: `pill ${tone}`, text: label })),
      el("td", { text: when(invitation.expires_at) }),
      el("td", { text: invitation.used_by || "—" }),
      el("td", { class: "row-actions" }, invitation.state === "pending"
        ? el("button", {
          type: "button", class: "small danger", text: "Revocar",
          "aria-label": `Revocar la invitación ${invitation.hint}`,
          onclick: () => revoke(invitation),
        })
        : null)));
  }
}

async function revoke(invitation) {
  try {
    await api("DELETE", `/admin/invitations/${invitation.id}`);
    notify("Invitación revocada.");
    await loadInvitations();
  } catch (err) {
    notify(err.message, true);
  }
}

$("#invitation-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.currentTarget;
  const data = Object.fromEntries(new FormData(form));
  formError(form, "");
  busy(form, true);
  try {
    const created = await api("POST", "/admin/invitations", { note: data.note || "", hours: Number(data.hours) });
    $("#created-value").textContent = created.code;
    $("#created-code").hidden = false;
    $("#copy-code").focus();
    form.reset();
    await loadInvitations();
  } catch (err) {
    formError(form, err.message);
  } finally {
    busy(form, false);
  }
});

$("#copy-code").addEventListener("click", async () => {
  const code = $("#created-value").textContent;
  try {
    await navigator.clipboard.writeText(code);
    notify("Código copiado.");
  } catch {
    const range = document.createRange();
    range.selectNodeContents($("#created-value"));
    getSelection().removeAllRanges();
    getSelection().addRange(range);
    notify("Selecciona y copia el código con Ctrl+C.");
  }
});

// --- Uso del relay ----------------------------------------------------------------------------

const monthFormat = new Intl.DateTimeFormat("es", { month: "long", year: "numeric", timeZone: "UTC" });

/// Los últimos 12 meses (UTC), del actual hacia atrás.
function fillMonths(select) {
  const now = new Date();
  for (let back = 0; back < 12; back += 1) {
    const date = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() - back, 1));
    const value = `${date.getUTCFullYear()}-${String(date.getUTCMonth() + 1).padStart(2, "0")}`;
    const label = monthFormat.format(date);
    select.append(el("option", { value, text: label.charAt(0).toUpperCase() + label.slice(1) }));
  }
}

async function loadUsage() {
  const input = $("#usage-month");
  if (!input.options.length) fillMonths(input);
  const usage = await api("GET", `/admin/usage?month=${encodeURIComponent(input.value)}`);
  $("#usage-total").textContent = bytes(usage.total);
  $("#usage-quota").textContent = usage.quota_bytes ? bytes(usage.quota_bytes) : "Sin límite";
  const body = $("#usage-body");
  body.replaceChildren();
  $("#usage-empty").hidden = usage.users.length > 0;
  const scale = usage.quota_bytes || Math.max(1, ...usage.users.map((user) => user.bytes));
  for (const user of usage.users) {
    const ratio = Math.min(1, user.bytes / scale);
    const over = usage.quota_bytes && user.bytes >= usage.quota_bytes;
    // Ancho por CSSOM: la política de contenido no admite atributos `style`.
    const fill = el("span");
    fill.style.width = `${(ratio * 100).toFixed(1)}%`;
    body.append(el("tr", {},
      el("td", { text: user.username }),
      el("td", {}, el("div", {
        class: `bar${over ? " over" : ""}`, role: "img",
        "aria-label": usage.quota_bytes ? `${Math.round(ratio * 100)} % de la cuota` : bytes(user.bytes),
      }, fill)),
      el("td", { class: "num", text: bytes(user.bytes) })));
  }
}

$("#usage-month").addEventListener("change", () => loadUsage().catch((err) => notify(err.message, true)));

// --- Salas ------------------------------------------------------------------------------------

async function loadRooms() {
  const { rooms } = await api("GET", "/admin/rooms");
  $("#rooms-count").textContent = rooms.length;
  $("#rooms-relayed").textContent = rooms.filter((room) => room.relayed).length;
  $("#rooms-guests").textContent = rooms.reduce((sum, room) => sum + room.guests, 0);
  const body = $("#rooms-body");
  body.replaceChildren();
  $("#rooms-empty").hidden = rooms.length > 0;
  rooms.forEach((room, index) => {
    body.append(el("tr", {},
      el("td", { text: `Sala ${index + 1}` }),
      el("td", { class: "num", text: room.guests }),
      el("td", {}, el("span", { class: `pill ${room.relayed ? "accent" : ""}`, text: room.relayed ? "Sí" : "Solo directo" })),
      el("td", { class: "num", text: bytes(room.relay_bytes) }),
      el("td", { class: "num", text: room.relay_dropped })));
  });
}

// --- Ajustes ----------------------------------------------------------------------------------

async function loadSettings() {
  const s = await api("GET", "/admin/settings");
  const tls = { "self-signed": "Autofirmado (solo desarrollo)", files: "Certificado del operador", acme: "ACME automático" }[s.tls_mode] || s.tls_mode;
  const rows = [
    ["Servidor", `${s.public_host} (UDP ${s.public_udp_port}, TCP ${s.public_tcp_port})`],
    ["Versión", `coroom-server ${s.version} · protocolo ${s.protocol}`],
    ["Certificado", tls],
    ["Directorio ACME", s.acme_directory],
    ["Reto ACME", s.acme_challenge],
    ["Huella del certificado", s.certificate_fingerprint],
    ["Caduca", s.certificate_not_after ? dateFormat.format(new Date(s.certificate_not_after)) : null],
    ["Alta de usuarios", s.registration === "invite" ? "El administrador o con código de invitación" : "Solo el administrador"],
    ["Relay", s.relay_enabled ? "Habilitado" : "Deshabilitado"],
    ["Cuota mensual del relay", s.relay_quota_gb_per_month > 0 ? `${s.relay_quota_gb_per_month} GB por anfitrión` : "Sin límite"],
    ["Límite por sala", s.relay_max_kbps > 0 ? `${(s.relay_max_kbps / 1000).toLocaleString("es")} Mbit/s` : "Sin límite"],
    ["Invitados por sala", s.max_guests_per_room],
    ["UDP alcanzable", s.udp_reachable === null ? "Sin comprobar todavía" : (s.udp_reachable ? "Sí" : "No (revisa el puerto UDP; una NAT sin hairpin también da «no»)")],
    ["Última comprobación", s.udp_checked_at ? dateFormat.format(new Date(s.udp_checked_at)) : null],
    ["Modo desarrollo", s.dev_mode ? "Sí: COROOM_DEV_TOKEN definido" : "No"],
  ];
  const list = $("#settings-list");
  list.replaceChildren();
  for (const [name, value] of rows) {
    if (value === null || value === undefined) continue;
    list.append(el("dt", { text: name }), el("dd", { text: value }));
  }
  // El servidor es AGPL: su código fuente se ofrece a quien lo usa (COROOM_SOURCE_URL).
  const source = el("a", { href: s.source_url, text: s.source_url });
  source.rel = "noopener";
  list.append(el("dt", { text: "Código fuente" }), el("dd", {}, source));
}

boot();
