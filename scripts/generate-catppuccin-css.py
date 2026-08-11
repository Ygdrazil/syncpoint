"""Regenerate the Catppuccin theme blocks in src/app.css.

Four flavours times nine variables is thirty-six hex values; transcribing those
by hand is how a palette quietly ends up wrong. This pulls them from the
official palette instead.

    python3 scripts/generate-catppuccin-css.py

Reads https://github.com/catppuccin/palette and rewrites the block between the
generated-region markers in src/app.css.
"""
import json
import sys
import urllib.request

PALETTE_URL = "https://raw.githubusercontent.com/catppuccin/palette/main/palette.json"
CSS_PATH = "src/app.css"
BEGIN = "/* catppuccin:begin */"
END = "/* catppuccin:end */"

FLAVOURS = ("latte", "frappe", "macchiato", "mocha")

# `base` is the lightest of base/mantle/crust in every flavour, including the
# light one, so using it for the panel keeps panels sitting above the
# background — the same relationship the built-in themes have.
VARIABLES = (
    ("--bg", "mantle"),
    ("--panel", "base"),
    ("--border", "surface0"),
    ("--text", "text"),
    ("--muted", "subtext0"),
    ("--accent", "blue"),
    ("--danger", "red"),
    ("--ok", "green"),
    ("--warn", "yellow"),
)


def fetch_palette():
    with urllib.request.urlopen(PALETTE_URL, timeout=30) as response:
        return json.load(response)


def render(palette):
    blocks = []
    for key in FLAVOURS:
        flavour = palette[key]
        colors = flavour["colors"]
        lines = [
            f':root[data-theme="catppuccin-{key}"] {{',
            f'  /* Catppuccin {flavour["name"]} */',
        ]
        for variable, name in VARIABLES:
            lines.append(f'  {variable}: {colors[name]["hex"]}; /* {name} */')
        # Drives the webview's own controls and scrollbars; Latte is the light one.
        lines.append(f'  color-scheme: {"dark" if flavour["dark"] else "light"};')
        lines.append("}")
        blocks.append("\n".join(lines))
    return "\n\n".join(blocks)


def main():
    css = open(CSS_PATH).read()
    if BEGIN not in css or END not in css:
        sys.exit(f"markers {BEGIN} / {END} not found in {CSS_PATH}")

    head, rest = css.split(BEGIN, 1)
    _, tail = rest.split(END, 1)
    body = render(fetch_palette())
    open(CSS_PATH, "w").write(f"{head}{BEGIN}\n{body}\n{END}{tail}")
    print(f"regenerated {len(FLAVOURS)} flavours in {CSS_PATH}")


if __name__ == "__main__":
    main()
