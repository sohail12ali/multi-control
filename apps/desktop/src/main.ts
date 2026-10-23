type DeskScreen = {
  id: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  placed: boolean;
};

type Peer = {
  id: string;
  name: string;
  host: string;
  port: number;
  paired: boolean;
  online: boolean;
};

type State = {
  local_id: string;
  local_name: string;
  paused: boolean;
  permission: { reason?: string } | "unknown" | "granted" | "denied" | { unsupported: { reason: string } } | string;
  focused_id: string;
  focused_name: string;
  controlling_remote: boolean;
  screens: DeskScreen[];
  discovered: Peer[];
  paired: { id: string; name: string }[];
  pairing: { kind: "showing_code"; peer_name: string; code: string } | { kind: "enter_code"; peer_name: string } | null;
};

type DeskRequest = { type: string; [key: string]: unknown };

let state: State = mockState();
let arranged = false;
let send: (request: DeskRequest) => void = (request) => mockRequest(request);
let expand: (on: boolean) => void = (on) => {
  document.getElementById("panel")?.classList.toggle("wide", on);
};

const colors = ["#f7d7b5", "#cfe7d8", "#d9def7", "#f6d3df", "#e7e0c8"];

function mockState(): State {
  return {
    local_id: "local",
    local_name: "This Mac",
    paused: false,
    permission: "granted",
    focused_id: "local",
    focused_name: "This Mac",
    controlling_remote: false,
    screens: [
      { id: "local", name: "This Mac", x: 0, y: 0, width: 1440, height: 900, placed: true },
      { id: "win", name: "Windows", x: 1440, y: 40, width: 1920, height: 1080, placed: true },
      { id: "linux", name: "Linux", x: 0, y: 0, width: 1280, height: 800, placed: false },
    ],
    discovered: [
      { id: "win", name: "Windows", host: "10.0.0.4", port: 47320, paired: true, online: true },
      { id: "linux", name: "Linux", host: "10.0.0.5", port: 47320, paired: true, online: true },
      { id: "new", name: "Office PC", host: "10.0.0.8", port: 47320, paired: false, online: true },
    ],
    paired: [
      { id: "win", name: "Windows" },
      { id: "linux", name: "Linux" },
    ],
    pairing: null,
  };
}

function mockRequest(request: DeskRequest) {
  if (request.type === "set_paused") state.paused = Boolean(request.paused);
  if (request.type === "set_name") state.local_name = String(request.name);
  if (request.type === "begin_add") {
    const peer = state.discovered.find((item) => item.id === request.peer_id);
    state.pairing = { kind: "showing_code", peer_name: peer?.name ?? "Computer", code: "482193" };
  }
  if (request.type === "submit_code") state.pairing = null;
  if (request.type === "cancel_pair") state.pairing = null;
  if (request.type === "place") {
    const screen = state.screens.find((item) => item.id === request.id);
    if (screen) {
      screen.x = Number(request.x);
      screen.y = Number(request.y);
      screen.placed = true;
    }
  }
  if (request.type === "unplace") {
    const screen = state.screens.find((item) => item.id === request.id);
    if (screen && screen.id !== state.local_id) screen.placed = false;
  }
  if (request.type === "jump") {
    state.focused_id = String(request.target);
    state.focused_name = state.screens.find((item) => item.id === request.target)?.name ?? state.focused_name;
    state.controlling_remote = state.focused_id !== state.local_id;
  }
  render();
}

function permissionText(permission: State["permission"]): string | null {
  if (permission === "granted" || permission === "unknown") return null;
  if (permission === "denied") return "Allow Accessibility for multicontrol-daemon, then come back.";
  if (typeof permission === "object" && permission && "unsupported" in permission) {
    return permission.unsupported.reason;
  }
  if (typeof permission === "object" && permission?.reason) return permission.reason;
  return null;
}

function side(anchor: DeskScreen, other: DeskScreen): string {
  if (!other.placed) return "Not placed";
  const dx = other.x + other.width / 2 - (anchor.x + anchor.width / 2);
  const dy = other.y + other.height / 2 - (anchor.y + anchor.height / 2);
  if (Math.abs(dx) >= Math.abs(dy)) return dx >= 0 ? "Right" : "Left";
  return dy >= 0 ? "Below" : "Above";
}

function render() {
  const name = document.querySelector<HTMLInputElement>("#name")!;
  if (document.activeElement !== name) name.value = state.local_name;
  const dot = document.querySelector<HTMLElement>("#dot")!;
  dot.className = state.paused ? "dot paused" : "dot on";
  document.querySelector("#status")!.textContent = state.paused
    ? "Paused on this computer"
    : `Controlling ${state.focused_name}`;
  document.querySelector("#pause")!.textContent = state.paused ? "Resume" : "Pause";

  const permission = document.querySelector<HTMLElement>("#permission")!;
  const note = permissionText(state.permission);
  permission.classList.toggle("hidden", !note);
  permission.innerHTML = note
    ? `<p>${note}</p><button id="settings" type="button" class="primary">Open settings</button>`
    : "";
  document.querySelector("#settings")?.addEventListener("click", () => send({ type: "open_permission_settings" }));

  const pairing = document.querySelector<HTMLElement>("#pairing")!;
  if (!state.pairing) {
    pairing.classList.add("hidden");
    pairing.innerHTML = "";
  } else if (state.pairing.kind === "showing_code") {
    pairing.classList.remove("hidden");
    pairing.innerHTML = `<p>On ${state.pairing.peer_name}, enter</p><p class="code">${state.pairing.code}</p><button id="cancel" type="button">Cancel</button>`;
    document.querySelector("#cancel")?.addEventListener("click", () => send({ type: "cancel_pair" }));
  } else {
    pairing.classList.remove("hidden");
    pairing.innerHTML = `<p>${state.pairing.peer_name} wants to connect.</p><input id="code" type="text" inputmode="numeric" placeholder="Code" /><div class="row"><button id="join" class="primary" type="button">Connect</button><button id="cancel" type="button">Cancel</button></div>`;
    document.querySelector("#join")?.addEventListener("click", () => {
      const code = document.querySelector<HTMLInputElement>("#code")!.value;
      send({ type: "submit_code", code });
    });
    document.querySelector("#cancel")?.addEventListener("click", () => send({ type: "cancel_pair" }));
  }

  const local = state.screens.find((screen) => screen.id === state.local_id);
  const machines = document.querySelector<HTMLElement>("#machines")!;
  machines.classList.toggle("hidden", arranged);
  machines.innerHTML = "";
  for (const peer of state.discovered.filter((item) => item.paired)) {
    const screen = state.screens.find((item) => item.id === peer.id);
    const row = document.createElement("li");
    row.className = "machine";
    row.innerHTML = `<div><strong>${peer.name}</strong><br><small>${screen && local ? side(local, screen) : "Not placed"}</small></div><span class="${peer.online ? "online" : "offline"}"></span>`;
    row.addEventListener("click", () => send({ type: "jump", target: peer.id }));
    machines.append(row);
  }

  document.querySelector("#arrange")!.classList.toggle("hidden", !arranged);
  if (arranged) drawDesk();
}

function drawDesk() {
  const canvas = document.querySelector<HTMLElement>("#canvas")!;
  const shelf = document.querySelector<HTMLElement>("#shelf")!;
  canvas.innerHTML = "";
  shelf.innerHTML = "";
  const placed = state.screens.filter((screen) => screen.placed);
  const bounds = placed.reduce(
    (box, screen) => ({
      x: Math.min(box.x, screen.x),
      y: Math.min(box.y, screen.y),
      r: Math.max(box.r, screen.x + screen.width),
      b: Math.max(box.b, screen.y + screen.height),
    }),
    { x: 0, y: 0, r: 1, b: 1 },
  );
  const pad = 24;
  const rect = canvas.getBoundingClientRect();
  const scale = Math.min((rect.width - pad * 2) / (bounds.r - bounds.x), (rect.height - pad * 2) / (bounds.b - bounds.y));
  placed.forEach((screen, index) => {
    const tile = document.createElement("div");
    tile.className = `tile${screen.id === state.local_id ? " local" : ""}`;
    tile.style.left = `${pad + (screen.x - bounds.x) * scale}px`;
    tile.style.top = `${pad + (screen.y - bounds.y) * scale}px`;
    tile.style.width = `${Math.max(screen.width * scale, 72)}px`;
    tile.style.height = `${Math.max(screen.height * scale, 48)}px`;
    tile.style.background = colors[index % colors.length];
    tile.innerHTML = `<div><strong>${screen.name}</strong><br><span>${screen.width}×${screen.height}</span></div>`;
    tile.addEventListener("pointerdown", (event) => drag(screen, event, canvas, bounds, scale, pad));
    canvas.append(tile);
  });
  for (const screen of state.screens.filter((item) => !item.placed)) {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "chip";
    chip.textContent = screen.name;
    chip.addEventListener("click", () => send({ type: "place", id: screen.id, x: bounds.r + 40, y: bounds.y }));
    shelf.append(chip);
  }
}

function drag(screen: DeskScreen, start: PointerEvent, canvas: HTMLElement, bounds: { x: number; y: number }, scale: number, pad: number) {
  const originX = screen.x;
  const originY = screen.y;
  const move = (event: PointerEvent) => {
    const dx = (event.clientX - start.clientX) / scale;
    const dy = (event.clientY - start.clientY) / scale;
    screen.x = Math.round(originX + dx);
    screen.y = Math.round(originY + dy);
    drawDesk();
  };
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    send({ type: "place", id: screen.id, x: screen.x, y: screen.y });
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  void canvas;
  void bounds;
  void pad;
}

function applyLine(line: string) {
  const message = JSON.parse(line) as { type: string; snapshot?: State; label?: string };
  if (message.type === "state" && message.snapshot) {
    state = message.snapshot;
    render();
  }
  if (message.type === "identify" && message.label) {
    const status = document.querySelector("#status");
    if (status) status.textContent = `This screen is ${message.label}`;
  }
  if (message.type === "error") {
    const status = document.querySelector("#status");
    if (status) status.textContent = String((message as { message?: string }).message ?? "Something went wrong");
  }
}

document.querySelector("#pause")!.addEventListener("click", () => send({ type: "set_paused", paused: !state.paused }));
document.querySelector("#name")!.addEventListener("change", (event) => {
  send({ type: "set_name", name: (event.target as HTMLInputElement).value });
});
document.querySelector("#add")!.addEventListener("click", () => {
  const found = state.discovered.find((peer) => !peer.paired);
  if (found) send({ type: "begin_add", peer_id: found.id });
  else {
    const host = window.prompt("Address", "127.0.0.1");
    if (!host) return;
    send({ type: "begin_add_address", host, port: 47320 });
  }
});
document.querySelector("#arrange-toggle")!.addEventListener("click", () => {
  arranged = !arranged;
  expand(arranged);
  document.querySelector("#arrange-toggle")!.textContent = arranged ? "Done" : "Arrange";
  render();
});

async function boot() {
  const tauri = (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  if (!tauri) {
    render();
    return;
  }
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  send = (request) => {
    void invoke("daemon_request", { request });
  };
  expand = (on) => {
    void invoke("set_expanded", { expanded: on });
  };
  await listen<string>("daemon", (event) => applyLine(event.payload));
}

void boot();
