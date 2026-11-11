# mmh3 studio

A native and web studio for generating pictures with
[MiniMax-H3-Image](https://huggingface.co/MiniMaxAI/MiniMax-H3-Image) through
[mmh3](https://github.com/niw/mmh3), built with [Rune](https://github.com/anomalyco/rune).

- `apps/studio`: the Rune app. See its [README](apps/studio/README.md).

The flake builds the studio and mmh3 itself, so neither needs a checkout:

```sh
nix run github:ryoppippi/mmh3-studio#mmh3 -- server   # with MMH3_MODELS set
nix run github:ryoppippi/mmh3-studio
```

Working in the repository, `direnv allow` loads the dev shell and `just` lists the tasks.

The model weights are not part of this repository and come under their own licence; check the
[MiniMax-H3-Image licence](https://huggingface.co/MiniMaxAI/MiniMax-H3-Image/blob/main/LICENSE),
including its territorial restrictions, before using them.
