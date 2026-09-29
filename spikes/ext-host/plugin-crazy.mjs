// plugin-crazy.mjs — hangs: ignores stdin, infinite busy loop (the "bad third-party plugin")
let sink = 0;
for (;;) {
  sink += Math.random();
  if (sink === Number.POSITIVE_INFINITY) sink = 0; // unreachable, defeats DCE
}
