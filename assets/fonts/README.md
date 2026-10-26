# Test fonts

`NotoSans-Regular-Latin.ttf` is Noto Sans Regular 2.015 cut down to Latin with
`hb-subset`, so tests and goldens never depend on the fonts a machine has
installed. It keeps every layout feature. Licence: SIL Open Font License 1.1, in
`OFL.txt`.

Made with:

```sh
hb-subset /usr/share/fonts/noto/NotoSans-Regular.ttf \
  --unicodes="U+0020-007E,U+00A0-017F,U+2010-2027,U+2030-203A,U+20AC,U+2122" \
  --layout-features='*' --output-file=NotoSans-Regular-Latin.ttf
```

The live overlay does not use this file. It finds the UI font through fontconfig at
run time.
