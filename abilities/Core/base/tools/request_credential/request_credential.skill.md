# request_credential

## Usage

- Request a private human credential form in the active conversation.
- Supply a plain key name and HTTPS service origin, plus a human label and the HTTPS page where the user creates the value when known.
- The form stays in this conversation until the user saves or dismisses it.
- The user enters the value directly; models and mini apps receive only metadata.
- Does not grant network access or silently use this credential.
