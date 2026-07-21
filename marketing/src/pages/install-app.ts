import installer from "../../../scripts/install-app.sh?raw";

export const prerender = true;

export function GET() {
  return new Response(`${installer.trimEnd()}\n`, {
    headers: {
      "Cache-Control": "public, max-age=300, s-maxage=300",
      "Content-Type": "text/x-shellscript; charset=utf-8",
    },
  });
}
