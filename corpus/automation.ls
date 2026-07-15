# song: automation-demo  tempo: 110.00  meter: 4/4  key: Am  grid: 1/16
# instruments: lead:81 pad:89
#bind cutoff = cc74
#bind lead.vib = bend
#bind macro = host:mix/lead
#bind pad.cutoff = cc74

P1 lead | a2 c2 e2 c2 a4 g4 |
  @cutoff { 0:20 8:110 smooth 25/2:80 16:60 }
  @vib { 8:0 10:1200 12:0 exp:2 14:-1200 16:0 }
P2 pad  | [ace]16 |
  @cutoff { 0:10 16:90 }
  @macro { 0:0 16:1 }

arrangement:
  A: [P1+P2] x2
