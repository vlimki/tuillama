fn role_tag(r: &Role) -> u8 {
    match r {
        Role::User => 1,
        Role::Assistant => 2,
        Role::System => 3,
    }
}

fn message_hash(m: &Message) -> u64 {
    let mut h = DefaultHasher::new();
    role_tag(&m.role).hash(&mut h);
    m.content.hash(&mut h);
    h.finish()
}

fn str_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn clone_with_modifier(mut t: Text<'static>, m: Modifier) -> Text<'static> {
    for line in &mut t.lines {
        for span in &mut line.spans {
            span.style = span.style.add_modifier(m);
        }
    }
    t
}

