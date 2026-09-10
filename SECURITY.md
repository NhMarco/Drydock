# Security

Do not commit activation-device files, access tokens, signing private keys, or update credentials.
Runtime-generated device keys live below the portable `settings` directory, which is ignored by Git.

The client may contain public verification or encryption keys. Corresponding private authority keys
must remain on the trusted service and must never be embedded in a release binary.

Report security issues privately to the repository owner rather than opening a public issue.
