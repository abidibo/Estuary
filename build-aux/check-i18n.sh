#!/bin/sh
# Flags string literals passed to user-visible GTK/adw setters without i18n()/ni18n_f()
# (UI M3.2.7 / M11.5). Heuristic: a setter call whose argument starts with a quote.
# Usage: build-aux/check-i18n.sh   (exit 1 when something is found)
cd "$(dirname "$0")/.." || exit 2
pattern='\.(set_title|set_label|set_subtitle|set_description|set_tooltip_text|set_body|set_heading|set_button_label)\((Some\()?&?"[A-Za-z]'
ctor='(Label|Button|Toast|Banner|AlertDialog)::(new|with_label)\((Some\()?&?"[A-Za-z]'
response='add_response\("[^"]*", *&?"[A-Za-z]'
found=$(grep -rnE "$pattern|$ctor|$response" crates/estuary/src --include='*.rs' | grep -v 'i18n' | grep -v '// i18n-ok')
if [ -n "$found" ]; then
    echo "Untranslated user-visible strings:"
    echo "$found"
    exit 1
fi
echo "i18n check: no untranslated user-visible literals found"
