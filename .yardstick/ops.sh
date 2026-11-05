#!/usr/bin/env bash
op_attach() { printf '%s\n' '(attach (options))'; }
op_config_file() { printf '%s\n' 'fux/config.grift'; }
op_cfg_bind_split() { printf '(bind-key "%s" (code! (lambda! () (split (axis (focused (me)) "horizontal")) #inert)))\n' "$1"; }
op_cfg_bind_line() { printf '(bind-key "%s" (code! (lambda! () (line (focused (me)) "%s") #inert)))\n' "$1" "$2"; }
op_cfg_bind_prompt_line() { printf '(bind-key "%s" (code! (lambda! () (prompt (me) "line" "" (code! (lambda! (line-value) (line (focused (me)) line-value)))))))\n' "$1"; }
op_cfg_set() { if [[ $2 == string ]]; then printf '(set "%s" "%s")\n' "$1" "$3"; elif [[ $2 == bool ]]; then printf '(set "%s" #%s)\n' "$1" "$3"; else printf '(set "%s" %s)\n' "$1" "$3"; fi; }
op_cfg_load() { printf '(include! "%s")\n' "$1"; }
op_cfg_defined() { printf '(sequence! %s #inert)\n' "$1"; }
op_prompt_split() { printf '%s\n' '(split (axis (focused (me)) "horizontal"))'; }
op_prompt_split_pane() { printf '(split (axis (pane "%s") "horizontal"))\n' "$1"; }
op_split() { op_prompt_split_pane "$1"; }
op_close() { printf '(kill (pane "%s"))\n' "$1"; }
op_send_line() { printf '(line (pane "%s") "%s")\n' "$1" "$2"; }
op_send_arg_line() { printf '(line (pane "%s") (first (arguments)))\n' "$1"; }
op_capture() { printf '(capture (pane "%s"))\n' "$1"; }
op_wait_text() { printf '(sequence! (shows (pane "%s") "%s" %s) #inert)\n' "$1" "$2" "$3"; }
op_pane_columns() { printf '(get (pane "%s") "columns")\n' "$1"; }
op_pane_record() { printf '(info (pane "%s"))\n' "$1"; }
op_pane_name() { printf '(get (pane "%s") "name")\n' "$1"; }
op_rename_pane() { printf '(rename (pane "%s") "%s")\n' "$1" "$2"; }
op_zoom() { printf '(zoom (pane "%s"))\n' "$1"; }
op_cheap_request() { printf '%s\n' '(sequence! (snapshot (all) "topology") #inert)'; }
op_save_session() { printf '%s\n' '(dump (all))'; }
op_restore_session() { printf '%s\n' '--file' "$1"; }
op_start_server() { printf '%s\n' '(start)'; }
op_stop_server() { printf '%s\n' '(kill-server)'; }
op_setting() { printf '(display (get (first (filter (lambda! (setting-value) (same? (get setting-value "name") "%s")) (preferences (get (first (get (list-session) "clients")) "id")))) "value"))\n' "$1"; }
op_list_keys() { printf '%s\n' '(list-keys)'; }
op_help() { printf '%s\n' '(help)'; }
op_help_split() { printf '%s\n' '(explain "split")'; }
op_help_topic() { printf '(explain "%s")\n' "$1"; }
op_defined() { printf '(sequence! %s #inert)\n' "$1"; }
