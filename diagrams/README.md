# Ablation Diagram Sources

These `.d2` files are the source for the flow / architecture diagrams
under `/images/ablation/flow/*.svg`. The compiled SVGs are checked in
so CI doesn't need `d2` installed.

To regenerate after editing a source file:

```sh
cd docs/ablation/diagrams
for f in *.d2; do
    d2 --layout=elk --theme=300 "$f" "../../public/images/ablation/flow/${f%.d2}.svg"
done
```

Install `d2` via `brew install d2` or from <https://d2lang.com/>.

Sequence diagrams (Hello/Welcome handshake, dedup PUT) stay inline in
markdown as Mermaid — the sequence output is acceptable and avoids
forcing a build-time tool for those.
