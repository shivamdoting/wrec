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
      weights: [400, 700],
      styles: ["normal"],
    },
    {
      provider: fontProviders.local(),
      name: "Geist",
      cssVariable: "--font-geist",
      options: {
        variants: [
          {
            src: ["./src/assets/fonts/geist/Geist[wght].ttf"],
            weight: "100 900",
            style: "normal",
          },
        ],
      },
    },
    {
      provider: fontProviders.local(),
      name: "Geist Mono",
      cssVariable: "--font-geist-mono",
      options: {
        variants: [
          {
            src: ["./src/assets/fonts/geist/GeistMono[wght].ttf"],
            weight: "100 900",
            style: "normal",
          },
        ],
      },
    },
  ],
});
