import { Resvg } from "@resvg/resvg-js";
import type { APIRoute } from "astro";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import satori from "satori";

// The social card is built here so it stays in sync with the site and needs no
// checked-in binary. It renders once at build time into dist/og.png.
const WIDTH = 1200;
const HEIGHT = 630;
const RED = "#c62828";

// Read from the project root, not import.meta.url: this module is bundled into
// dist/ before it runs, so a module-relative path points at nothing.
const departureMono = await readFile(
  resolve(
    process.cwd(),
    "src/assets/fonts/departure/DepartureMono-Regular.otf",
  ),
);

const card = {
  type: "div",
  props: {
    style: {
      width: WIDTH,
      height: HEIGHT,
      display: "flex",
      flexDirection: "column",
      padding: "64px 72px",
      background: RED,
      color: "#fff",
      fontFamily: "Departure Mono",
    },
    children: [
      {
        type: "div",
        props: {
          children: "wrec",
          style: { fontSize: 132, lineHeight: 1, letterSpacing: -2 },
        },
      },
      {
        type: "div",
        props: {
          children: "the efficient, agent-native",
          style: { fontSize: 38, lineHeight: 1.3, marginTop: 28 },
        },
      },
      {
        type: "div",
        props: {
          children: "screen recorder for macos",
          style: { fontSize: 38, lineHeight: 1.3 },
        },
      },
    ],
  },
};

export const GET: APIRoute = async () => {
  const svg = await satori(card as never, {
    width: WIDTH,
    height: HEIGHT,
    fonts: [
      {
        name: "Departure Mono",
        data: departureMono,
        weight: 400,
        style: "normal",
      },
    ],
  });

  const png = new Resvg(svg, {
    fitTo: { mode: "width", value: WIDTH },
  })
    .render()
    .asPng();

  return new Response(new Uint8Array(png), {
    headers: { "Content-Type": "image/png" },
  });
};
