# Security policy

FastFind parses untrusted files (documents, archives, PDFs), so parser bugs can be security
bugs. Please report vulnerabilities privately through GitHub's
**Security → Report a vulnerability** on this repository instead of opening a public issue.

Include the affected version, the platform and, where possible, a file that triggers the
problem. You'll get a reply within a week.

In scope: crashes, hangs or memory exhaustion caused by a crafted file that escapes the
existing limits (size, time, zip-bomb and nesting guards); reading or opening files outside
indexed folders; any network access; and the Tauri IPC surface.

Supported versions: the latest release.
