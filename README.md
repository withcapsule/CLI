# Capsule CLI

This is the command-line client for Capsule. It can upload files, download them by ID, inspect status, delete them from the server, and optionally encrypt files locally before upload.

## Install from source

```sh
cargo build --release
./target/release/capsule --help
```

For development (compiles a debug variant):

```sh
cargo run
```

## Main commands

- `ping` - test server connectivity
- `upload` - upload a file
- `upload-encrypted` - encrypt locally, then upload
- `download` - download a file by ID
- `status` - show metadata for a file
- `recents` - show recent transfers
- `delete` - delete a file from the server
- `server` - view or change the configured server

The CLI also supports shell completions:

```sh
capsule completions bash
capsule completions zsh
capsule completions fish
...etc
```

## Server selection

You can override the server per command:

```sh
capsule --server http://localhost:9001 ping
```

Or store it for later:

```sh
capsule server set http://localhost:9001
```

## Local state

The CLI stores a small amount of local state under the platform data directory:

- `capsule/server.txt` - saved server address
- `capsule/history.json` - recent uploads and downloads

## Encryption

`upload-encrypted` performs encryption on the client before the file is sent to the server. The server only stores the encrypted file and a flag that it was uploaded in encrypted form.
