# Command wire migration (section 5)

Chosen before editing consumers. Values below are illustrative entity bits;
real callers use the IDs returned by the server. `order` remains `previous` or
`next`, `scope` is `tab` or `workspace`, and subject remains externally tagged.
Action names in settings/keybindings do not change.

| Old command JSON | New command JSON |
| --- | --- |
| `{"kind":"tab_close","tab":42}` | `{"kind":"close","subject":{"tab":42}}` |
| `{"kind":"workspace_close","workspace":42}` | `{"kind":"close","subject":{"workspace":42}}` |
| `{"kind":"tab_select","tab":42}` | `{"kind":"select","scope":"tab","entity":42}` |
| `{"kind":"workspace_select","workspace":42}` | `{"kind":"select","scope":"workspace","entity":42}` |
| `{"kind":"tab_next"}` | `{"kind":"next","scope":"tab"}` |
| `{"kind":"workspace_next"}` | `{"kind":"next","scope":"workspace"}` |
| `{"kind":"tab_previous"}` | `{"kind":"previous","scope":"tab"}` |
| `{"kind":"workspace_previous"}` | `{"kind":"previous","scope":"workspace"}` |
| `{"kind":"tab_reorder","order":"next"}` | `{"kind":"reorder","scope":"tab","order":"next"}` |
| `{"kind":"workspace_reorder","order":"previous"}` | `{"kind":"reorder","scope":"workspace","order":"previous"}` |
| `{"kind":"reorder","order":"next"}` | `{"kind":"reorder_pane","order":"next"}` |
| `{"kind":"move_to_tab","tab":42}` | `{"kind":"move","to":{"kind":"tab","tab":42}}` |
| `{"kind":"move_to_new_tab","name":null}` | `{"kind":"move","to":{"kind":"new_tab","name":null}}` |
| `{"kind":"move_to_workspace","workspace":42}` | `{"kind":"move","to":{"kind":"workspace","workspace":42}}` |
| `{"kind":"move_to_new_workspace","name":"work"}` | `{"kind":"move","to":{"kind":"new_workspace","name":"work"}}` |

No legacy command aliases. `TabNew`/`WorkspaceNew`, directional moves, and
interactive chooser tags remain unchanged. `MoveTo` becomes the shared
internally tagged struct-variant type with `kind` as discriminator. Both new
wire types are registered with reflection as well as deriving serde.
