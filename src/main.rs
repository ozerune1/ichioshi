struct Rectangle {
    width: u32,
    height: u32
}

impl Rectangle {
    fn area(&self) -> u32 {
        self.width * self.height
    }
}

fn main() {
    let x = Rectangle {
        width: 30,
        height: 5
    };

    println!("The area of the rectangle is {}", x.area());

}
