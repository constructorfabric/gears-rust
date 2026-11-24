# md-fabric

Output folder for the interactive map of the repository's Markdown files and the links between them. Generate it with:

```sh
make md-fabric
```

The target runs `tools/scripts/md-fabric.py --inline-data` and writes a single self-contained page, `docs/md-fabric/md-fabric.html`, with the graph data embedded. Do not commit it. It is not covered by `.gitignore`, so `git status` lists it as untracked after a run.
