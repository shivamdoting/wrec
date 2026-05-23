// @ts-check
import tailwindcss from "@tailwindcss/vite";
import { defineConfig, fontProviders } from "astro/config";

// https://astro.build/config
export default defineConfig({
  vite: {
    plugins: [tailwindcss()],
  },
  fonts: [
    {
      provider: fontProviders.fontshare(),
      name: "Clash Grotesk",
      cssVariable: "--font-clash-grotesk",
      weights: [700],
      styles: ["normal"],
    },
  ],
});
