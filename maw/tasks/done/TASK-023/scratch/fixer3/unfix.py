# Puts the pre-fix swap_remove back into Live::turn_end (restore with the .orig copy).
import sys, shutil
p='crates/cctg/src/hub/stream.rs'
if sys.argv[1]=='apply':
    shutil.copyfile(p, p+'.orig')
    t=open(p,encoding='utf-8',newline='').read()
    old="""        if self.answered_ends.contains(&end) {
            return None;
        }"""
    new="""        if let Some(at) = self.answered_ends.iter().position(|&known| known == end) {
            self.answered_ends.swap_remove(at);
            return None;
        }"""
    nl='\r\n' if '\r\n' in t else '\n'
    old=old.replace('\n',nl); new=new.replace('\n',nl)
    assert t.count(old)==1
    open(p,'w',encoding='utf-8',newline='').write(t.replace(old,new))
else:
    shutil.move(p+'.orig', p)
