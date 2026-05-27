const ffi = globalThis.KiraBrowserFFI;
const root = ffi.documentBody();
const title = ffi.createElement("h1");
ffi.setText(title, "Kira WebGPU surface");
ffi.appendChild(root, title);
const canvas = ffi.createCanvas();
ffi.setAttribute(canvas, "width", "640");
ffi.setAttribute(canvas, "height", "360");
ffi.setStyle(canvas, "border", "1px solid #222");
ffi.appendChild(root, canvas);
const status = ffi.createElement("p");
ffi.setText(status, "Detecting WebGPU");
ffi.appendChild(root, status);
ffi.detectWebGPU().then((info) => {
  ffi.setText(status, info.available && info.adapter ? "WebGPU adapter detected" : "WebGPU unavailable in this browser");
  ffi.consoleLog("Kira WebGPU capability detection completed");
}).catch((error) => {
  ffi.setText(status, "WebGPU detection failed");
  throw error;
});
