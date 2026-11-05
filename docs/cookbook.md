# fux recipes in grift

These examples use an already selected private server through `FUX_SOCKET`.

## A command and its actual exit status

```sh
fux '(define! p (new-tab (argument (argument (program (size (in (options) "batch") 80 24) "/bin/sh") "-c") "printf result=42; exit 7"))) (rename p "batch-process")'
fux '(get (get (until-ended (pane "batch-process") 10) "program") "exited")'
fux '(kill (workspace "batch"))'
```

## Shell arguments remain strings

```sh
fux '(define! p (new-tab (program (size (in (options) "quoted") 80 24) "/bin/sh"))) (rename p "quoted-shell")'
fux '(line (pane "quoted-shell") (first (arguments)))' 'echo "parentheses (stay data)"'
fux '(shows (pane "quoted-shell") "parentheses (stay data)" 10)'
fux '(kill (workspace "quoted"))'
```

## A repeating resize map

```grift
(bind-key "C-b r l" (code! (lambda! () (move-border (focused (me)) "right" 1))))
(label "grow right")
(group "Resize")
(repeat "C-b r")
```

## A prompt callback

```grift
(bind-key "C-b p" (code! (lambda! ()
  (prompt (me) "command" "" (code! (lambda! (line-value)
    (line (focused (me)) line-value)))))))
(label "type a command")
```
