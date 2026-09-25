// TASK-034 fixer: expected Claude Code project folder names for tail::tests.
// wX/Le/k/dx are copied verbatim from the Claude Code 2.1.28x binary
// (~/.local/bin/claude.exe, `function dx(e)` next to `var pte=200`).
// Run: node claude_folder_name.js
function wX(t){let e=0;for(let n=0;n<t.length;n++)e=(e<<5)-e+t.charCodeAt(n)|0;return e}
var pte=200;function Le(e){return Math.abs(wX(e)).toString(36)}function k(e){return e.replace(/[^a-zA-Z0-9]/g,"-")}function dx(e){let n=k(e);if(n.length<=pte)return n;return`${n.slice(0,pte)}-${Le(e)}`}

const B = String.fromCharCode(92); // a backslash, spelled so no shell eats it
const inputs = [
  "C:" + B + "work" + B + "x".repeat(220),
  "/home/я/" + "проект-😀-".repeat(30),
  "D:" + B + "y".repeat(230),
  "C:" + B + "Users" + B + "a_b" + B + "my dev.x",
];
for (const i of inputs) {
  console.log(JSON.stringify({ input: i, name: dx(i), hash: wX(i) }));
}
