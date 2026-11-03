"""Map vo-scene tests and vo runner files to the effect families they use.

Usage: tests_map.py <vo crates dir>   (prints JSON)
"""
import sys,os,re,json
CR=sys.argv[1]
sys.argv=['x',CR]
import importlib.util
spec=importlib.util.spec_from_file_location('sv',os.path.join(os.path.dirname(os.path.abspath(__file__)),'survey.py'));sv=importlib.util.module_from_spec(spec);spec.loader.exec_module(sv)
scene=sv.load(sv.SCENE); mods={sv.modname(r):r for r in scene}
COMMON={'Line','Sheet','Hold','Anchor','Element','Key','Point','Color','Rect','Size','Shape','Target','Edges','Place','Bell','Spine','Envelope','Reading','Caret','Flow'}
# fx public type names per family
names={}
for m,rel in mods.items():
    its,_=sv.items_of(scene[rel])
    for n,s,e in its:
        t=sv.target_of(m,n)
        if t in sv.FX and n[0].isupper() and n not in COMMON:
            line=scene[rel].split('\n')[s]
            if line.lstrip().startswith(('pub struct','pub enum','pub trait','pub type')):
                names[n]=sv.FX[t]
# Element methods tied to a family: pub fns in element.rs whose span names fx types or modules
methods={}
src=scene['element.rs']
for mm in re.finditer(r'\n    pub fn ([a-z_0-9]+)[^{;]*\{', src):
    start=mm.start();
    nxt=src.find('\n    pub fn ',mm.end()); body=src[start: nxt if nxt>0 else len(src)]
    fam=set()
    for n,f in names.items():
        if re.search(r'\b%s\b'%n,body): fam.add(f)
    for m in mods:
        t=sv.DEFAULT[m]
        if t in sv.FX and re.search(r'crate::%s::'%re.escape(m.split('/')[-1]),body): fam.add(sv.FX[t])
    if len(fam)==1: methods[mm.group(1)]=fam.pop()
# Live text draws through flow, so a test of live text tests the text family.
methods['live_text']='text'
def fams(src):
    out={}
    clean=sv.strip_comments(src)
    for pfx in ('vo_scene','scene'):
        for p,_ in sv.refs_with_line(clean+'\n',pfx):
            if p[0] in mods:
                pm,it=sv.resolve(p,mods); t=sv.target_of(pm,it)
                if t in sv.FX: out.setdefault(sv.FX[t],set()).add(pm)
    for n,f in names.items():
        if re.search(r'\b%s\b'%n,clean): out.setdefault(f,set()).add(n)
    for n,f in methods.items():
        if re.search(r'\.%s\('%n,clean) or re.search(r'Element::%s\b'%n,clean): out.setdefault(f,set()).add('.'+n+'()')
    return out
res={}
for d in ['vo-scene/tests','vo/src','vo/tests','vo/examples']:
    p=os.path.join(CR,d)
    for dp,_,fs in os.walk(p):
        for f in sorted(fs):
            if f.endswith('.rs'):
                fp=os.path.join(dp,f)
                res[os.path.relpath(fp,CR)]={k:sorted(v) for k,v in fams(open(fp).read()).items()}
json.dump({'methods':methods,'files':res},sys.stdout,indent=0)
