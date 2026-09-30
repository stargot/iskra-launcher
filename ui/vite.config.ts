import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Iskra UI (Фаза 1, шаг 1). Tauri dev ожидает devUrl http://localhost:5173 (tauri.conf.json).
export default defineConfig({
  plugins: [react()],
  // Не глушить вывод cargo при `tauri dev`.
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
});
