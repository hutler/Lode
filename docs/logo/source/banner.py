"""Pixel banner: the mascot plus LODE. Run from docs/logo: python source/banner.py"""
from PIL import Image
K=(30,24,46,255); Y=(246,196,69,255); y=(201,141,30,255); W=(255,248,200,255)
F={'L':["X....","X....","X....","X....","X....","X....","XXXXX"],
   'O':[".XXX.","X...X","X...X","X...X","X...X","X...X",".XXX."],
   'D':["XXXX.","X...X","X...X","X...X","X...X","X...X","XXXX."],
   'E':["XXXXX","X....","X....","XXXX.","X....","X....","XXXXX"]}
S=3  # font pixel size in base pixels
sprite=Image.open('lode-mascot-64.png')
Wd=64+8+4*(5*S+S)+4; H=64
base=Image.new('RGBA',(Wd,H),(0,0,0,0))
base.alpha_composite(sprite,(0,0))
txt=Image.new('RGBA',(Wd,H),(0,0,0,0))
x0=72; y0=(H-7*S)//2+4
for i,ch in enumerate('LODE'):
    for r,row in enumerate(F[ch]):
        for c,v in enumerate(row):
            if v=='X':
                col = W if r==0 else (y if r>=5 else Y)
                for a in range(S):
                    for b in range(S):
                        txt.putpixel((x0+i*6*S+c*S+a, y0+r*S+b), col)
# outline text
out=txt.copy()
for yy in range(H):
    for xx in range(Wd):
        if txt.getpixel((xx,yy))[3]==0:
            if any(0<=xx+dx<Wd and 0<=yy+dy<H and txt.getpixel((xx+dx,yy+dy))[3] for dx in (-1,0,1) for dy in (-1,0,1)):
                out.putpixel((xx,yy),K)
# drop shadow
for yy in range(H-1,0,-1):
    pass
base.alpha_composite(out)
base.resize((Wd*4,H*4),Image.NEAREST).save('lode-banner.png')
