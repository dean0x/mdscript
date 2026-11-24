use super::*;
use crate::data::decl::parse_declarations;
use crate::evaluator::budget::MemBudget;
use crate::resolver::parse_frontmatter_yaml;

const MODULE: &str = "sub/main.mds";

fn parse(raw: &str, module: &str) -> Result<Vec<Entry>, MdsError> {
    let tree = parse_frontmatter_yaml(&format!("data: {raw}")).unwrap();
    parse_declarations(&tree["data"], module, &mut MemBudget::default())
}

fn nodes<'a>(entries: &'a [Entry], module: &'a str) -> Vec<Node<'a>> {
    entries
        .iter()
        .enumerate()
        .map(|(ordinal, entry)| Node {
            entry,
            position: (module, ordinal),
            module,
            dir: ".",
        })
        .collect()
}

fn check<'a>(
    nodes: &'a [Node<'a>],
    frontmatter: &[&str],
    runtime: &[&str],
) -> Result<Graph<'a>, MdsError> {
    analyze(
        nodes,
        |name| frontmatter.contains(&name),
        |name| runtime.contains(&name),
    )
}

#[test]
fn graph_resolves_entry_and_variable_roots() {
    let entries = parse(
        "{remote: 'https://example.test/{{group.id}}/{{region.name}}/{{local.slug}}', local: ./bad.yaml}",
        MODULE,
    )
    .unwrap();
    let nodes = nodes(&entries, MODULE);
    let graph = check(&nodes, &["group"], &["region"]).unwrap();
    assert_eq!(graph.frontier().collect::<Vec<_>>(), [(MODULE, "remote")]);
}

#[test]
fn graph_refuses_an_unknown_root_in_the_declaring_module() {
    let entries = parse(
        "{local: ./bad.yaml, remote: 'https://example.test/{{group.id}}'}",
        MODULE,
    )
    .unwrap();
    let nodes = nodes(&entries, MODULE);
    assert!(check(&nodes, &["group"], &[]).is_ok());
    let error = check(&nodes, &[], &[]).unwrap_err().serialize();
    assert_eq!(error.code, "mds::data");
    assert!(error
        .message
        .contains("unknown data reference name \"group\""));
    assert!(error.message.contains(MODULE));
    assert!(error.message.contains("entry \"remote\""));
    assert!(error.span.is_none());
}

#[test]
fn graph_refuses_self_and_two_node_cycles() {
    for (target, back) in [("first.id", "file.id"), ("second.id", "first.id")] {
        let raw = format!("{{file: ./bad.yaml, first: 'https://example.test/{{{{{target}}}}}', second: 'https://example.test/{{{{{back}}}}}'}}");
        let entries = parse(&raw, MODULE).unwrap();
        let nodes = nodes(&entries, MODULE);
        assert!(check(&nodes, &[], &["first"]).is_ok());
        let error = check(&nodes, &[], &[]).unwrap_err().serialize();
        assert_eq!(error.code, "mds::data");
        assert!(error.message.contains("data dependency cycle"));
    }
}

fn chain(count: usize, tail: &str, reverse: bool) -> Vec<Entry> {
    let mut fields = (1..count)
        .map(|i| format!("entry{i}: 'https://example.test/{{{{entry{}.id}}}}'", i + 1))
        .collect::<Vec<_>>();
    fields.push(format!("entry{count}: {tail}"));
    if reverse {
        fields.reverse();
    }
    parse(&format!("{{{}}}", fields.join(", ")), MODULE).unwrap()
}

#[test]
fn graph_caps_the_longest_path_in_either_declaration_order() {
    for reverse in [false, true] {
        let at = chain(16, "./leaf.json", reverse);
        assert!(check(&nodes(&at, MODULE), &[], &[]).is_ok());
        let over = chain(17, "./leaf.json", reverse);
        let error = check(&nodes(&over, MODULE), &[], &[])
            .unwrap_err()
            .serialize();
        assert_eq!(error.code, "mds::resource_limit");
        assert!(error.message.contains("16 levels"));
    }
}

#[test]
fn graph_refuses_a_cycle_longer_than_the_level_cap() {
    let entries = chain(17, "'https://example.test/{{entry1.id}}'", false);
    let nodes = nodes(&entries, MODULE);
    assert!(check(&nodes, &[], &["entry1", "entry2"]).is_ok());
    let error = check(&nodes, &[], &[]).unwrap_err().serialize();
    assert_eq!(error.code, "mds::data");
    assert!(error.message.contains("data dependency cycle"));
}

#[test]
fn replay_satisfies_file_and_url_nodes_without_their_dependencies() {
    let entries = parse(
        "{local: ./bad.yaml, remote: 'https://example.test/{{absent}}', dependent: 'https://example.test/{{remote.id}}/{{local.id}}'}",
        MODULE,
    )
    .unwrap();
    let nodes = nodes(&entries, MODULE);
    assert_eq!(
        check(&nodes, &[], &[]).unwrap_err().serialize().code,
        "mds::data"
    );
    let graph = check(&nodes, &[], &["local", "remote"]).unwrap();
    assert_eq!(
        graph
            .unreplayed()
            .map(|node| node.entry.name.as_str())
            .collect::<Vec<_>>(),
        ["dependent"]
    );
    assert_eq!(
        graph.frontier().collect::<Vec<_>>(),
        [(MODULE, "dependent")]
    );
}

#[test]
fn replay_cannot_skip_static_validation() {
    let collect = |raw| {
        let entries = parse(raw, MODULE)?;
        let nodes = nodes(&entries, MODULE);
        Ok::<_, MdsError>(check(&nodes, &[], &["remote"])?.frontier().count())
    };
    assert_eq!(
        collect("{remote: 'https://example.test/{{path}}'}").unwrap(),
        0
    );
    let error = collect("{remote: 'https://{{host}}/path'}")
        .unwrap_err()
        .serialize();
    assert_eq!(error.code, "mds::data");
    assert!(error.message.contains("literal URL authority"));
}

#[test]
fn frontier_keeps_module_and_declaration_order() {
    let first = parse("{z: {url: 'https://example.test/{{a.id}}'}, a: 'https://example.test/path', file: ./leaf.json, replay: 'https://example.test/path'}", "z.mds").unwrap();
    let second = parse(
        "{a: 'https://example.test/path', replay: ./leaf.json}",
        "a.mds",
    )
    .unwrap();
    let first_nodes = nodes(&first, "z.mds");
    let second_nodes = nodes(&second, "a.mds");
    let graphs = [
        check(&first_nodes, &[], &["replay"]).unwrap(),
        check(&second_nodes, &[], &["replay"]).unwrap(),
    ];
    let frontier = graphs.iter().flat_map(Graph::frontier).collect::<Vec<_>>();
    assert_eq!(frontier, [("z.mds", "z"), ("z.mds", "a"), ("a.mds", "a")]);
    assert_eq!(
        MdsError::network_denied(&frontier).serialize().code,
        "mds::network_denied"
    );
}

#[test]
fn merged_graph_accepts_the_compile_entry_cap_but_not_more() {
    // A merged scope can have more entries than one module can declare.
    let cap = MAX_DATA_ENTRIES_PER_COMPILE;
    let last = format!("entry{}", cap - 1);
    let mut entries = (0..=cap)
        .map(|i| Entry {
            name: format!("entry{i}"),
            source: Source::File("./leaf.json".into()),
        })
        .collect::<Vec<_>>();
    entries[0].source = Source::Url(vec![format!("{last}.id"), format!("{last}.name")]);
    let at_cap = nodes(&entries, MODULE);
    let graph = check(&at_cap[..cap], &[], &[]).unwrap();
    assert_eq!(graph.unreplayed().count(), cap);
    assert_eq!(
        check(&at_cap, &[], &[]).unwrap_err().serialize().code,
        "mds::resource_limit"
    );

    entries[cap - 1].source = Source::Url(vec!["entry0.id".into()]);
    let cycle = nodes(&entries[..cap], MODULE);
    let error = check(&cycle, &[], &[]).unwrap_err().serialize();
    assert_eq!(error.code, "mds::data");
    assert!(error.message.contains("data dependency cycle"));
}
