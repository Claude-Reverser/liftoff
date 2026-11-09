// spotlight's calculator is fine actually. still rewriting it. no daemon needed for 2+2.

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i] == b' ' {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat(b'+') {
                v += self.term()?;
            } else if self.eat(b'-') {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }
    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') || self.eat(b'x') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                v /= self.unary()?;
            } else if self.eat(b'%') {
                v %= self.unary()?;
            } else {
                return Some(v);
            }
        }
    }
    fn unary(&mut self) -> Option<f64> {
        if self.eat(b'-') {
            return Some(-self.unary()?);
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.pow()
    }
    fn pow(&mut self) -> Option<f64> {
        let b = self.atom()?;
        if self.eat(b'^') {
            return Some(b.powf(self.unary()?));
        }
        Some(b)
    }
    fn atom(&mut self) -> Option<f64> {
        let c = self.peek()?;
        if c == b'(' {
            self.i += 1;
            let v = self.expr()?;
            self.eat(b')');
            return Some(v);
        }
        if c.is_ascii_digit() || c == b'.' {
            let st = self.i;
            while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                self.i += 1;
            }
            let mut v: f64 = std::str::from_utf8(&self.s[st..self.i]).ok()?.parse().ok()?;
            if self.peek() == Some(b'e') && self.s.get(self.i + 1).is_some_and(|c| c.is_ascii_digit() || *c == b'-') {
                self.i += 1;
                let neg = self.eat(b'-');
                let st = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                let e: i32 = std::str::from_utf8(&self.s[st..self.i]).ok()?.parse().ok()?;
                v *= 10f64.powi(if neg { -e } else { e });
            }
            return Some(v);
        }
        if c.is_ascii_alphabetic() {
            let st = self.i;
            while self.i < self.s.len() && self.s[self.i].is_ascii_alphanumeric() {
                self.i += 1;
            }
            let id = std::str::from_utf8(&self.s[st..self.i]).ok()?;
            match id {
                "pi" => return Some(std::f64::consts::PI),
                "e" => return Some(std::f64::consts::E),
                "tau" => return Some(std::f64::consts::TAU),
                _ => {}
            }
            let a = self.atom()?;
            return Some(match id {
                "sqrt" => a.sqrt(),
                "cbrt" => a.cbrt(),
                "sin" => a.sin(),
                "cos" => a.cos(),
                "tan" => a.tan(),
                "asin" => a.asin(),
                "acos" => a.acos(),
                "atan" => a.atan(),
                "ln" => a.ln(),
                "log" => a.log10(),
                "log2" => a.log2(),
                "abs" => a.abs(),
                "floor" => a.floor(),
                "ceil" => a.ceil(),
                "round" => a.round(),
                "exp" => a.exp(),
                _ => return None,
            });
        }
        None
    }
}

pub fn eval(q: &str) -> Option<String> {
    let q = q.trim().trim_start_matches('=').trim();
    let has_digit = q.bytes().any(|b| b.is_ascii_digit());
    let has_op = q.bytes().any(|b| b"+-*/^%()x".contains(&b)) || q.chars().any(|c| c.is_ascii_alphabetic());
    if !has_digit || !has_op || q.len() > 200 {
        return None;
    }
    let s = q.replace(['×', '·'], "*").replace('÷', "/").replace(',', ".").to_lowercase();
    let mut p = P { s: s.as_bytes(), i: 0 };
    let v = p.expr()?;
    if p.peek().is_some() || !v.is_finite() {
        return None;
    }
    if q.parse::<f64>().is_ok() {
        return None;
    }
    Some(fmt(v))
}

fn fmt(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    if v.abs() >= 1e15 || v.abs() < 1e-6 {
        return format!("{v:.6e}");
    }
    let s = format!("{v:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::eval;
    #[test]
    fn math() {
        assert_eq!(eval("2+2").as_deref(), Some("4"));
        assert_eq!(eval("2 + 3 * 4").as_deref(), Some("14"));
        assert_eq!(eval("(2+3)*4").as_deref(), Some("20"));
        assert_eq!(eval("2^10").as_deref(), Some("1024"));
        assert_eq!(eval("-2^2").as_deref(), Some("-4"));
        assert_eq!(eval("10/4").as_deref(), Some("2.5"));
        assert_eq!(eval("sqrt(16)").as_deref(), Some("4"));
        assert_eq!(eval("1,5*2").as_deref(), Some("3"));
        assert_eq!(eval("1/0"), None);
        assert_eq!(eval("safari"), None);
        assert_eq!(eval("42"), None);
        assert_eq!(eval("2 x 3").as_deref(), Some("6"));
        assert_eq!(eval("v2").as_deref(), None);
    }
}
