"""Parse `crate::` and `vo_scene::` use trees out of Rust source (a text scan, not rustc)."""
import re,os,sys,json
def strip_comments(s):
    s=re.sub(r'//[^\n]*','',s)
    return s
def parse_tree(s,i):
    # parse use-tree starting at s[i] after '::'; returns list of paths, new index
    paths=[]
    s2=s[i:]
    if s2.startswith('{'):
        depth=0;j=0
        for j,ch in enumerate(s2):
            if ch=='{':depth+=1
            elif ch=='}':
                depth-=1
                if depth==0:break
        inner=s2[1:j]
        # split top-level commas
        parts=[];d=0;cur=''
        for ch in inner:
            if ch=='{':d+=1
            if ch=='}':d-=1
            if ch==',' and d==0: parts.append(cur);cur=''
            else: cur+=ch
        parts.append(cur)
        for p in parts:
            p=p.strip()
            if not p:continue
            m=re.match(r'([A-Za-z_][A-Za-z0-9_]*)(::)?',p)
            if not m: continue
            head=m.group(1)
            if m.group(2):
                sub,_=parse_tree(p,m.end())
                paths+=[[head]+x for x in sub]
            else: paths.append([head])
        return paths,i+j+1
    m=re.match(r'([A-Za-z_][A-Za-z0-9_]*)(::)?',s2)
    if not m: return [],i
    head=m.group(1)
    if m.group(2):
        sub,k=parse_tree(s,i+m.end())
        return [[head]+x for x in sub] or [[head]],k
    return [[head]],i+m.end()
def refs(src,prefix):
    src=strip_comments(src)
    out=[]
    for m in re.finditer(r'\b'+prefix+r'::',src):
        ps,_=parse_tree(src,m.end())
        out+=ps
    return out
if __name__=='__main__':
    root,prefix=sys.argv[1],sys.argv[2]
    res={}
    for dp,_,fs in os.walk(root):
        for f in fs:
            if f.endswith('.rs'):
                p=os.path.join(dp,f);rel=os.path.relpath(p,root)
                src=open(p).read()
                k=src.find('#[cfg(test)]\nmod tests')
                body=src if k<0 else src[:k]
                res[rel]={'main':refs(body,prefix),'test':refs(src[k:],prefix) if k>=0 else []}
    json.dump(res,sys.stdout)
