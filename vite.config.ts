import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import pkg from "./package.json";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  envPrefix: ["VITE_", "TAURI_"],
  server: { port: 5173, strictPort: true },
  build: { target: "es2021", sourcemap: false },
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
});
