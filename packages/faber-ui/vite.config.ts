import path from "node:path"
import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"
import { tanstackRouter } from "@tanstack/router-plugin/vite"

export default defineConfig({
  resolve: {
    // Mirrors tsconfig "@/*": ["./*"] — "@" maps to the package root.
    alias: { "@": path.resolve(import.meta.dirname, ".") },
  },
  plugins: [
    tanstackRouter({ target: "react", autoCodeSplitting: true }),
    react(),
    tailwindcss(),
  ],
  server: {
    port: 3000,
    proxy: {
      "/api": { target: "http://127.0.0.1:3001", changeOrigin: true },
    },
  },
})
