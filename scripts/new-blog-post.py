#!/usr/bin/env python3
"""Create a new Zola blog post under blog/content/posts/.

Usage: new-blog-post.py "Some Title"

Writes `blog/content/posts/YYYY-MM-DD-<slug>.md` with TOML front
matter (title + ISO-8601 timestamp) and creates a matching image
directory at `blog/static/img/YYYY-MM-DD-<slug>/` where per-post
images live (referenced from the post as `/img/YYYY-MM-DD-<slug>/
<file>`). Also stamps a "write to think" planning sibling at
`blog/plans/YYYY-MM-DD-<slug>.md`, copied from
`blog/plans/_template.md`, which the author fills out before
writing the post itself. The plan lives outside `blog/content/`
so Zola never renders it.

If the post path already exists (i.e. another post with the same
slug was made earlier today), appends -2, -3, ... until it finds
an unused stem; the image directory and plan file mirror that
stem. Prints the final post path to stdout.
"""
import argparse
import datetime
import re
import sys
import unicodedata
from pathlib import Path


def slugify(title: str) -> str:
    normalized = unicodedata.normalize("NFKD", title)
    ascii_only = normalized.encode("ascii", "ignore").decode("ascii")
    lowered = ascii_only.lower()
    return re.sub(r"[^a-z0-9]+", "-", lowered).strip("-")


def toml_escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace('"', '\\"')


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("title", help="Human-readable post title")
    args = parser.parse_args()

    slug = slugify(args.title)
    if not slug:
        print("error: title produced empty slug", file=sys.stderr)
        return 2

    now = datetime.datetime.now().astimezone()
    date_str = now.date().isoformat()
    timestamp = now.replace(microsecond=0).isoformat()

    repo_root = Path(__file__).resolve().parent.parent
    posts_dir = repo_root / "blog" / "content" / "posts"
    posts_dir.mkdir(parents=True, exist_ok=True)

    path = posts_dir / f"{date_str}-{slug}.md"
    stem = f"{date_str}-{slug}"
    suffix = 2
    while path.exists():
        stem = f"{date_str}-{slug}-{suffix}"
        path = posts_dir / f"{stem}.md"
        suffix += 1

    img_dir = repo_root / "blog" / "static" / "img" / stem
    img_dir.mkdir(parents=True, exist_ok=True)

    front_matter = (
        "+++\n"
        f'title = "{toml_escape(args.title)}"\n'
        f"date = {timestamp}\n"
        'authors = ["Claude"]\n'
        "+++\n"
        "\n"
        "<!-- more -->\n"
    )
    path.write_text(front_matter)

    plans_dir = repo_root / "blog" / "plans"
    plans_dir.mkdir(parents=True, exist_ok=True)
    template_path = plans_dir / "_template.md"
    plan_path = plans_dir / f"{stem}.md"
    if template_path.exists() and not plan_path.exists():
        body = template_path.read_text().replace("<post stem>", stem, 1)
        plan_path.write_text(body)

    print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
