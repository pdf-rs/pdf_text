use itertools::Itertools;
use pdf::file::FileOptions;

fn main() {
    let input = std::env::args_os().nth(1).expect("no file given");
    let file = FileOptions::cached().open(&input).expect("can't read PDF");
    let resolver = file.resolver();
    
    for (page_nr, page) in file.pages().enumerate() {
        let page = page.expect("can't read page");
        let flow = pdf_text::run(&file, &page, &resolver).expect("can't render page");
        println!("# page {}", page_nr + 1);
        for run in flow.runs {
            for line in run.lines {
                println!("{}", line.words.iter().map(|w| &w.text).format(" "));
            }
            println!();
        }
    }
}
