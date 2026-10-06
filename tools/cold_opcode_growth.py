"""Patch a COPY of the tree with six cold opcodes, for the dispatch-growth check.

    python3 tools/cold_opcode_growth.py <copy-of-repo> [--common]

The opcodes decode from tags 200-205, pass validation, and have real handlers,
so the compiler cannot drop them. No benchmark executes them. By default they
are rare opcodes: arms of `exec_rare`. With `--common` they are dispatched by
`exec` to handlers of their own, like the common operations. Build the bench
in the copy and in an unpatched copy and compare the workloads; see
docs/PERFORMANCE.md. Never run this on the working tree.
"""
import os
import sys

if os.path.exists(os.path.join(sys.argv[1], '.git')):
    sys.exit('refusing to patch a git checkout; pass an exported copy')
root=sys.argv[1]+'/crates/moonseed/src/'
common='--common' in sys.argv[2:]
def ed(f, pairs):
    s=open(root+f).read()
    for old,new in pairs:
        assert old in s,(f,old[:50]); s=s.replace(old,new,1)
    open(root+f,'w').write(s)
N=6
variants=''.join(f"    Dummy{i} {{ dst: u8, k: u16 }},\n" for i in range(N))
enc=''.join(f"            Self::Dummy{i} {{ dst, k }} => {{ out.push({200+i}); out.push(dst); out.extend(k.to_le_bytes()); }}\n" for i in range(N))
dec=''.join(f"            {200+i} => Self::Dummy{i} {{ dst: read_u8(input)?, k: read_u16(input)? }},\n" for i in range(N))
pat='|'.join(f"Op::Dummy{i} {{ dst, k }}" for i in range(N))
ed('opcode.rs',[("    Halt,\n}", "    Halt,\n"+variants+"}"),
  ("            Self::Halt => out.push(20),","            Self::Halt => out.push(20),\n"+enc),
  ("            20 => Self::Halt,\n", "            20 => Self::Halt,\n"+dec)])
ed('check.rs',[("        Op::Halt => Ok(()),","        Op::Halt => Ok(()),\n        "+pat+" => { need(max, dst)?; constant(spec, k) }")])
ed('program.rs',[("            Op::Halt => {}","            Op::Halt => {}\n            "+pat.replace(', k',', ..')+" => self.touch(dst),")])
body=''.join(f"""            Op::Dummy{i} {{ dst, k }} => {{
                let bytes = self.const_bytes(k)?;
                let mut acc = {i}i64;
                for b in bytes {{ acc = acc.wrapping_mul(31).wrapping_add(i64::from(b)); }}
                let handle = self.alloc_table()?;
                self.store(dst, Value::Table(handle))?;
                self.store(dst.wrapping_add(1), Value::Integer(acc))?;
            }}
""" for i in range(N))
if not common:
    ed('runtime.rs',[("            Op::Halt => {\n", body+"            Op::Halt => {\n")])
else:
    arms=''.join(f"            Op::Dummy{i} {{ dst, k }} => self.op_dummy{i}(dst, k),\n" for i in range(N))
    handlers=''.join(f"""    #[inline(never)]
    fn op_dummy{i}(&mut self, dst: u8, k: u16) -> Result<Poll, VmError> {{
        let bytes = self.const_bytes(k)?;
        let mut acc = {i}i64;
        for b in bytes {{ acc = acc.wrapping_mul(31).wrapping_add(i64::from(b)); }}
        let handle = self.alloc_table()?;
        self.store(dst, Value::Table(handle))?;
        self.store(dst.wrapping_add(1), Value::Integer(acc))?;
        self.next_op()
    }}

""" for i in range(N))
    routed=''.join(f"            | Op::Dummy{i} {{ .. }}\n" for i in range(N))
    ed('runtime.rs',[
        ("            _ => self.exec_rare(op, journal),\n", arms+"            _ => self.exec_rare(op, journal),\n"),
        ("    /// Opcodes outside the common set", handlers+"    /// Opcodes outside the common set"),
        ("            | Op::ForPrep { .. } => return Err(VmError::Corrupt),", routed+"            | Op::ForPrep { .. } => return Err(VmError::Corrupt),"),
    ])
