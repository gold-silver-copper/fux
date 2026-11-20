#!/bin/sh
# xterm's "printer" for the referee: printer.sh DIR. xterm's text of its screen is what it
# prints. The print the player asks for at the end is kept as DIR/printed (DIR/final is there
# by then); what a case prints itself goes aside.
if [ -e "$1/final" ]; then f=$1/printed; else f=$1/early; fi
cat > "$f.new" && mv "$f.new" "$f"
