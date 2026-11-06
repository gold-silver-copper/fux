#!/bin/sh
printf '\033]2;build\007'
for i in 1 2 3; do echo "line $i"; sleep 0.3; done
for p in 10 20 30 40 50 60 70 80 90 100; do printf '\rprogress %3d%%' $p; sleep 0.05; done; echo
printf '\007'; sleep 0.3
tput clear; echo "after clear"; sleep 0.3
tput smcup; echo "in vi-like app"; sleep 0.3; tput rmcup
seq 1 60 | sed 's/^/row /'
