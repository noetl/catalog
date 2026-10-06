# Wiki pages, staged here until the wiki is seeded

`noetl/catalog`'s wiki is **enabled** (`has_wiki: true`) but its git repository does
not exist yet: GitHub creates `…/catalog.wiki.git` only when the **first page is
saved through the web UI**. Verified with a positive control —
`git ls-remote git@github.com:noetl/server.wiki.git` resolves, the same command
against `catalog.wiki.git` returns `Repository not found`.

So the pages live here until someone saves one page in the UI. Then:

```bash
git clone git@github.com:noetl/catalog.wiki.git
cp docs/wiki/Home.md catalog.wiki/Home.md
cd catalog.wiki && git add -A && git commit -m "docs: the catalog wiki Home" && git push
```

Tracked as part of noetl/ai-meta#427. This directory is staging, not a second home —
once the wiki is seeded, the wiki is the source of truth and these copies should be
deleted rather than maintained in parallel. Two copies of a document is the drift
shape `agents/rules/representation-drift.md` is about.
