use itertools::Itertools;
use pdf::file::FileOptions;

fn main() {
    let input = std::env::args_os().nth(1).expect("no file given");
    let file = FileOptions::cached().open(&input).expect("can't read PDF");
    let resolver = file.resolver();
    
    // for (page_nr, page) in file.pages().enumerate() {
        let page: pdf::object::PageRc = file.get_page(0).unwrap();
        let flow = pdf_text::run(&file, &page, &resolver, Default::default(), false).expect("can't render page");

        println!("# page {}", 0 + 1);
        for run in flow.runs {
            for line in run.lines {
                for w in line.words {
                    println!("{}", w.text);
                }
            }
        }
        for line in flow.lines {
            for w in line.words {
                println!("{}", w.text);
            }
        }
    // }
}
