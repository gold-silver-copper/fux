#!/bin/sh
# Types into an interactive bash a key at a time, as a person would.
sleep 0.5
for k in e c h o ' ' h i; do printf %s "$k"; sleep 0.15; done
printf '\r'; sleep 0.4
for k in l s x; do printf %s "$k"; sleep 0.15; done
printf '\177'; sleep 0.3; printf ' /\r'; sleep 0.5
printf 'exit\r'; sleep 0.3
