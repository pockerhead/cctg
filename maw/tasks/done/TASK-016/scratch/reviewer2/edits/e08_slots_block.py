# Replaces the stream block of slots.rs (from `fn release` up to the doc
# comment of `on_reply`) with slots_block.rs.
import os
here = os.path.dirname(os.path.abspath(__file__))
p = os.path.join(here, '..', 'ws', 'crates', 'cctg', 'src', 'hub', 'slots.rs')
s = open(p, encoding='utf-8').read()
start = s.index('    fn release(&mut self, session: &str, held: Held) {')
end = s.index("    /// Sends an agent's reply to the topic of its session: the chunks of")
block = open(os.path.join(here, 'slots_block.rs'), encoding='utf-8').read()
s = s[:start] + block + '\n' + s[end:]
open(p, 'w', encoding='utf-8', newline='\n').write(s)
print('ok')
