# Dim

Dim is a Rust coding agent with a terminal scrollback interface.

Install the Rust alpha with Node.js `^22.19.0 || >=23.11.0` and optional dependencies enabled:

```sh
npm install -g dimcode-alpha@alpha
dima
```

The alpha installs only `dima` and uses `~/.dimcode-alpha`. It can coexist with the earlier CLI; sessions and account files are not automatically migrated.

The `dimcode` package and `dim` command remain available:

```sh
npm install -g dimcode
dim
```

The Rust CLI has not been published to `dimcode` yet. That package currently distributes the earlier TypeScript CLI. Use `dimcode-alpha@alpha` for this Rust implementation.

The platform package contains the native CLI and App Server; installation does not download executables or run install scripts.

Use `dima --help` for configuration and session commands. Platform packages are built for macOS arm64/x64, Linux arm64/x64 and Windows arm64/x64. Availability depends on verified release artifacts.

TypeScript plugins can import named-service types with
`import type { PluginFiles, Settings, Sessions, AuxiliaryModel, ToolRegistry, SessionMethods, GuideEntries } from 'dimcode-alpha/plugin'`.
See the [plugin author's guide](https://github.com/arcships/dim/blob/main/docs/typescript-plugin-authoring.md).
The type-only entry adds no runtime code or plugin registration.

Dim's original source uses MIT. See `NOTICE` for distribution attribution. Native platform packages include `licenses/RUST-THIRD-PARTY-NOTICES.txt`, terminal library licenses under `licenses/grok-build/`, and Aimux's own MIT license under `licenses/aimux/`. The optional `dimcode-web` package includes `web/THIRD-PARTY-NOTICES.txt`, covering its JavaScript dependencies and Geist fonts under their own licenses. Bundled plugins retain the license files supplied by their npm packages.

### Share an App Server

On the computer you want to use, open `/settings` and select **Remote access**. Turn on **Allow connections to this computer**. The panel shows the computer name, reachable addresses and the complete sharing token. Select the token to read it on a narrow screen. Sharing starts off and follows the background App Server when you exit the TUI.

```text
Target computer                          Computer at the keyboard
/settings -> Remote access               dima share discover
  Allow connections to this computer     dima share connect <address>
  Computer name                            Sharing token: _
  Sharing token                            [ordinary Dim terminal]
```

You can also enter a Tailscale IP or DNS name directly. Addresses without a port use 7443. The client remembers a successful pairing on this computer. Use `--no-remember` for a one-time connection. To run one command, use `dima share connect <address> -- mcp list`. After a token reset, enter the new one with `dima share connect <address> --pair`. Resetting the token or turning sharing off disconnects existing remote terminals. Remote access in Settings always controls the computer at the keyboard.
