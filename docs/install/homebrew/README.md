# Homebrew distribution

onesync ships via a third-party tap (a separate GitHub repository whose name
starts with `homebrew-`). Users install with:

```sh
brew tap <owner>/onesync          # adds the tap
brew install onesync              # builds + symlinks /usr/local/bin/onesync*
brew services start onesync       # registers a launchd job
```

## Tap repository layout

Create a second repo named `homebrew-onesync` under the same owner as the
main `onesync` repository. Its layout:

```
homebrew-onesync/
├── Formula/
│   └── onesync.rb            # ← copy of onesync.rb maintained here
└── README.md                 # tap install instructions
```

Homebrew auto-discovers the `Formula/` directory and exposes every `.rb`
file in it as an installable formula under the tap namespace.

## Per-release update flow

Each release produced by `.github/workflows/release.yml` emits a
`onesync-<version>-macos-universal.tar.gz` plus a side-by-side `.sha256`.
The tap copy of `onesync.rb` needs three fields refreshed:

1. `version "<new>"`
2. `url "https://github.com/<owner>/onesync/releases/download/v<new>/onesync-<new>-macos-universal.tar.gz"` (both `on_arm` and `on_intel` blocks)
3. `sha256 "<hex from .sha256 file>"`

The simplest way to refresh them is `brew bump-formula-pr`, which opens a
PR on the tap repo:

```sh
brew bump-formula-pr \
  --version=0.1.0 \
  --sha256="$(curl -fsSL https://github.com/<owner>/onesync/releases/download/v0.1.0/onesync-0.1.0-macos-universal.tar.gz.sha256 | awk '{print $1}')" \
  --no-browse \
  --url="https://github.com/<owner>/onesync/releases/download/v0.1.0/onesync-0.1.0-macos-universal.tar.gz" \
  onesync/onesync
```

For full automation, add a job to the release workflow that runs the same
command after the `softprops/action-gh-release@v2` step, using a fine-grained
PAT scoped to the tap repository (`OWNER_TAP_REPO_PAT`). The current
`release.yml` deliberately stops at publishing the GitHub Release so the
bookkeeping doesn't fail and lose the cert-signed artefacts; wiring the
tap bump is a one-line follow-up once the tap repo exists.

## First-time tap bootstrap

```sh
gh repo create <owner>/homebrew-onesync --public --description "Homebrew tap for onesync"
git clone https://github.com/<owner>/homebrew-onesync
cp ../onesync/docs/install/homebrew/onesync.rb homebrew-onesync/Formula/onesync.rb
cd homebrew-onesync
git add Formula/onesync.rb
git commit -m "Initial onesync formula"
git push
```

Then publish the first GitHub Release on the main repo and run
`brew bump-formula-pr` (above) so the formula points at real artefacts.
