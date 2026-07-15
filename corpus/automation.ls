# song: automation-demo  tempo: 110.00  meter: 4/4  key: Am  grid: 1/16
# instruments: lead:81 pad:89
#bind cutoff = cc74 [0..1]
#bind lead.vib = bend [-2..2]
#bind macro = host:mix/lead

P1 lead | a2 c2 e2 c2 a4 g4 |
  @cutoff { 0:0.2 8:1 smooth 25/2:0.6 16:0.4 }
  @vib { 8:0 10:1 12:0 bez:0.7,0,1,1 14:-1 16:0 }
P2 pad  | [ace]16 |
  @cutoff { 0:0.1 16:0.9 }
  @macro { 0:0 16:1 }

arrangement:
  A: [P1+P2] x2
